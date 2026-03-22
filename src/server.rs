//! WebSocket Proxy Server
//!
//! Listens for WebSocket connections and forwards data to a TCP target.

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

use crate::config::{ConfigChange, ConfigWatcher, ResolvedConfig, ServerFileConfig};
use crate::error::{Error, Result};

/// A lazy address resolver that resolves the address when a connection is established.
///
/// This allows DNS changes to be picked up without restarting the server.
#[derive(Clone)]
pub struct Address {
    resolver: Arc<dyn Fn() -> std::io::Result<SocketAddr> + Send + Sync>,
}

impl Address {
    /// Create a new lazy address from anything that implements `ToSocketAddrs`.
    ///
    /// The address will be resolved each time `resolve()` is called.
    pub fn new<T>(addr: T) -> Self
    where
        T: ToSocketAddrs + Clone + Send + Sync + 'static,
    {
        Address {
            resolver: Arc::new(move || {
                addr.clone().to_socket_addrs()?.next().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "could not resolve address")
                })
            }),
        }
    }

    /// Resolve the address.
    ///
    /// This performs DNS resolution if the address is a hostname.
    pub fn resolve(&self) -> std::io::Result<SocketAddr> {
        (self.resolver)()
    }
}

impl std::fmt::Debug for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Address").finish_non_exhaustive()
    }
}

impl From<String> for Address {
    fn from(s: String) -> Self {
        Address::new(s)
    }
}

impl From<&'static str> for Address {
    fn from(s: &'static str) -> Self {
        Address::new(s.to_string())
    }
}

impl From<SocketAddr> for Address {
    fn from(addr: SocketAddr) -> Self {
        Address::new(addr)
    }
}

/// TLS configuration for the server
#[derive(Debug, Clone)]
pub enum TlsConfig {
    /// Load certificate and key from files
    Files {
        /// Path to the certificate file (PEM format)
        cert_path: String,
        /// Path to the private key file (PEM format)
        key_path: String,
    },
    /// Generate a self-signed certificate
    SelfSigned,
}

/// TLS mode for the run() convenience function
#[derive(Debug, Clone)]
pub enum TlsMode<'a> {
    /// No TLS (plain WebSocket)
    None,
    /// Load certificate and key from files
    Files { cert: &'a str, key: &'a str },
    /// Generate a self-signed certificate
    SelfSigned,
}

/// Run a proxy server with the given configuration.
///
/// This is a convenience function that builds and runs a `ProxyServer`.
///
/// # Arguments
///
/// * `listen` - Address to listen for WebSocket connections (e.g., "0.0.0.0:8080")
/// * `routes` - Route mappings in "path=target" format (e.g., "/ssh=127.0.0.1:22")
/// * `default_target` - Default target for paths that don't match any route
/// * `tls` - TLS mode (None, Files, or SelfSigned)
///
/// # Example
///
/// ```no_run
/// # async fn example() -> wsproxy::Result<()> {
/// use wsproxy::server::TlsMode;
///
/// wsproxy::server::run(
///     "0.0.0.0:8080",
///     &["/ssh=127.0.0.1:22".to_string()],
///     Some("127.0.0.1:22"),
///     TlsMode::None,
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn run(
    listen: &str,
    routes: &[String],
    default_target: Option<&str>,
    tls: TlsMode<'_>,
) -> Result<()> {
    let mut builder = ProxyServer::builder();

    // Add routes
    for r in routes {
        let (path, target) = r.split_once('=').ok_or_else(|| {
            Error::config(format!(
                "Invalid route format '{}', expected 'path=target'",
                r
            ))
        })?;
        builder = builder.route(path.to_string(), target.to_string());
    }

    // Set default target if provided
    if let Some(target) = default_target {
        builder = builder.default_target(target.to_string());
    }

    // Set TLS config if provided
    let is_tls = !matches!(tls, TlsMode::None);
    match tls {
        TlsMode::None => {}
        TlsMode::Files { cert, key } => {
            builder = builder.tls(cert, key);
        }
        TlsMode::SelfSigned => {
            builder = builder.tls_self_signed();
        }
    }

    let server = builder.bind(listen)?;

    if is_tls {
        eprintln!("Proxy server listening on {} (WSS)", listen);
    } else {
        eprintln!("Proxy server listening on {}", listen);
    }

    server.run().await
}

/// Run a proxy server with configuration loaded from a file.
///
/// This function watches the config file for changes and hot-reloads
/// routing configuration. If the listen address or TLS settings change,
/// the server will restart (existing connections continue until complete).
///
/// # Arguments
///
/// * `config_path` - Path to the TOML configuration file
///
/// # Example Configuration
///
/// ```toml
/// listen = "0.0.0.0:8080"
/// default_target = "127.0.0.1:22"
///
/// [routes]
/// "/ssh" = "127.0.0.1:22"
/// "/db" = "127.0.0.1:5432"
///
/// [tls]
/// cert = "cert.pem"
/// key = "key.pem"
/// ```
pub async fn run_with_config(config_path: impl AsRef<Path>) -> Result<()> {
    let config_path = config_path.as_ref();

    loop {
        // Load initial config
        let config = ServerFileConfig::load(config_path)?;
        let resolved = ResolvedConfig::from_file_config(&config)?;

        // Build the server
        let tls_acceptor = build_tls_acceptor(&config)?;

        let is_tls = config.has_tls();
        if is_tls {
            eprintln!(
                "Proxy server listening on {} (WSS) - config: {}",
                config.listen,
                config_path.display()
            );
        } else {
            eprintln!(
                "Proxy server listening on {} - config: {}",
                config.listen,
                config_path.display()
            );
        }

        // Create shared routing config for hot-reload
        let shared_config = Arc::new(RwLock::new(resolved));

        // Set up config file watcher
        let mut watcher = ConfigWatcher::new(config_path, config.clone())?;

        // Bind the listener
        let listener = TcpListener::bind(&config.listen).await?;

        // Run the server with hot-reload support
        let restart_needed =
            run_server_loop(listener, tls_acceptor, shared_config, &mut watcher).await?;

        if !restart_needed {
            break;
        }

        eprintln!("Configuration changed, restarting server...");
    }

    Ok(())
}

fn build_tls_acceptor(config: &ServerFileConfig) -> Result<Option<tokio_rustls::TlsAcceptor>> {
    if !config.has_tls() {
        return Ok(None);
    }

    let (certs, key) = if config.tls.self_signed {
        generate_self_signed_cert()?
    } else if let (Some(cert), Some(key)) = (&config.tls.cert, &config.tls.key) {
        load_certs_from_files(cert, key)?
    } else {
        return Err(Error::config("invalid TLS configuration"));
    };

    let tls_config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| Error::config(format!("failed to create TLS config: {}", e)))?;

    Ok(Some(tokio_rustls::TlsAcceptor::from(Arc::new(tls_config))))
}

async fn run_server_loop(
    listener: TcpListener,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    shared_config: Arc<RwLock<ResolvedConfig>>,
    watcher: &mut ConfigWatcher,
) -> Result<bool> {
    loop {
        tokio::select! {
            // Accept new connections
            accept_result = listener.accept() => {
                let (stream, peer_addr) = accept_result?;
                let config = Arc::clone(&shared_config);
                let tls = tls_acceptor.clone();

                tokio::spawn(async move {
                    let result = if let Some(ref tls_acceptor) = tls {
                        match tls_acceptor.accept(stream).await {
                            Ok(tls_stream) => handle_ws_connection_shared(tls_stream, config).await,
                            Err(e) => {
                                eprintln!("TLS handshake failed from {}: {}", peer_addr, e);
                                return;
                            }
                        }
                    } else {
                        handle_ws_connection_shared(stream, config).await
                    };

                    if let Err(e) = result {
                        eprintln!("Error handling connection from {}: {}", peer_addr, e);
                    }
                });
            }

            // Handle config changes
            change = watcher.recv() => {
                match change {
                    Some(ConfigChange::RoutingOnly(new_config)) => {
                        // Hot-reload: just update the routing config
                        match ResolvedConfig::from_file_config(&new_config) {
                            Ok(resolved) => {
                                let mut config = shared_config.write().await;
                                *config = resolved;
                                eprintln!("Configuration reloaded (routing updated)");
                            }
                            Err(e) => {
                                eprintln!("Failed to apply config: {}", e);
                            }
                        }
                    }
                    Some(ConfigChange::FullRestart(_)) => {
                        // Need to restart - return true to signal restart
                        return Ok(true);
                    }
                    Some(ConfigChange::Error(e)) => {
                        eprintln!("Config reload error: {}", e);
                    }
                    None => {
                        // Watcher closed
                        return Ok(false);
                    }
                }
            }
        }
    }
}

/// Builder for creating a `ProxyServer`.
///
/// # Example
///
/// ```no_run
/// use wsproxy::ProxyServer;
///
/// # async fn example() -> wsproxy::Result<()> {
/// // Simple server with a single default target
/// let server = ProxyServer::builder()
///     .default_target("127.0.0.1:22")
///     .bind("0.0.0.0:8080")?;
///
/// // Server with multiple routes
/// let server = ProxyServer::builder()
///     .route("/ssh", "127.0.0.1:22")
///     .route("/db", "127.0.0.1:5432")
///     .route("/redis", "127.0.0.1:6379")
///     .bind("0.0.0.0:8080")?;
///
/// // Server with TLS (WSS)
/// let server = ProxyServer::builder()
///     .default_target("127.0.0.1:22")
///     .tls("cert.pem", "key.pem")
///     .bind("0.0.0.0:8443")?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct ProxyServerBuilder {
    routes: HashMap<String, Address>,
    default_target: Option<Address>,
    tls_config: Option<TlsConfig>,
}

impl ProxyServerBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a route mapping a URL path to a TCP target address.
    ///
    /// The target address is resolved when connections are established,
    /// allowing for DNS changes to be picked up without restarting.
    pub fn route<T>(mut self, path: impl Into<String>, target: T) -> Self
    where
        T: ToSocketAddrs + Clone + Send + Sync + 'static,
    {
        self.routes.insert(path.into(), Address::new(target));
        self
    }

    /// Set the default target for paths that don't match any route.
    ///
    /// The target address is resolved when connections are established,
    /// allowing for DNS changes to be picked up without restarting.
    pub fn default_target<T>(mut self, target: T) -> Self
    where
        T: ToSocketAddrs + Clone + Send + Sync + 'static,
    {
        self.default_target = Some(Address::new(target));
        self
    }

    /// Enable TLS (WSS) with the given certificate and key files.
    ///
    /// Both files should be in PEM format. The certificate file may contain
    /// the full certificate chain.
    pub fn tls(mut self, cert_path: impl Into<String>, key_path: impl Into<String>) -> Self {
        self.tls_config = Some(TlsConfig::Files {
            cert_path: cert_path.into(),
            key_path: key_path.into(),
        });
        self
    }

    /// Enable TLS (WSS) with an automatically generated self-signed certificate.
    ///
    /// The certificate will be valid for "localhost" and 127.0.0.1.
    /// This is useful for development/testing but should not be used in production.
    pub fn tls_self_signed(mut self) -> Self {
        self.tls_config = Some(TlsConfig::SelfSigned);
        self
    }

    /// Build the `ProxyServer` bound to the given address.
    ///
    /// # Arguments
    ///
    /// * `listen_addr` - The address to listen for WebSocket connections.
    ///
    /// # Errors
    ///
    /// Returns an error if neither routes nor a default target is configured.
    pub fn bind(self, listen_addr: impl ToSocketAddrs) -> Result<ProxyServer> {
        let listen_addr = resolve_addr(listen_addr)?;

        if self.routes.is_empty() && self.default_target.is_none() {
            return Err(Error::config(
                "at least one route or a default_target is required",
            ));
        }

        // Build TLS acceptor if TLS is configured
        let tls_acceptor = if let Some(tls_config) = &self.tls_config {
            let (certs, key) = match tls_config {
                TlsConfig::Files {
                    cert_path,
                    key_path,
                } => load_certs_from_files(cert_path, key_path)?,
                TlsConfig::SelfSigned => generate_self_signed_cert()?,
            };

            let config = tokio_rustls::rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .map_err(|e| Error::config(format!("failed to create TLS config: {}", e)))?;

            Some(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
        } else {
            None
        };

        Ok(ProxyServer {
            inner: Arc::new(ProxyServerInner {
                listen_addr,
                routes: self.routes,
                default_target: self.default_target,
                tls_acceptor,
            }),
        })
    }
}

fn resolve_addr(addr: impl ToSocketAddrs) -> Result<SocketAddr> {
    addr.to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::config("could not resolve address"))
}

fn load_certs_from_files(
    cert_path: &str,
    key_path: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    use std::io::BufReader;

    let cert_file = std::fs::File::open(cert_path).map_err(|e| {
        Error::config(format!(
            "failed to open TLS certificate '{}': {}",
            cert_path, e
        ))
    })?;
    let key_file = std::fs::File::open(key_path)
        .map_err(|e| Error::config(format!("failed to open TLS key '{}': {}", key_path, e)))?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| Error::config(format!("failed to parse TLS certificate: {}", e)))?;

    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| Error::config(format!("failed to parse TLS key: {}", e)))?
        .ok_or_else(|| Error::config("no private key found in key file"))?;

    Ok((certs, key))
}

fn generate_self_signed_cert() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyUsagePurpose, SanType};

    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    params.subject_alt_names = vec![
        SanType::DnsName(
            "localhost"
                .try_into()
                .map_err(|e| Error::config(format!("failed to create SAN: {}", e)))?,
        ),
        SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))),
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];

    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| Error::config(format!("failed to generate key pair: {}", e)))?;

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| Error::config(format!("failed to generate self-signed certificate: {}", e)))?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(key_pair.serialize_der())
        .map_err(|e| Error::config(format!("failed to serialize private key: {}", e)))?;

    Ok((vec![cert_der], key_der))
}

struct ProxyServerInner {
    listen_addr: SocketAddr,
    routes: HashMap<String, Address>,
    default_target: Option<Address>,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
}

/// A WebSocket proxy server that forwards WebSocket connections to TCP.
#[derive(Clone)]
pub struct ProxyServer {
    inner: Arc<ProxyServerInner>,
}

impl ProxyServer {
    /// Create a new builder for configuring a `ProxyServer`.
    pub fn builder() -> ProxyServerBuilder {
        ProxyServerBuilder::new()
    }

    /// Run the proxy server.
    ///
    /// This will listen for WebSocket connections and forward data to the configured TCP target.
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(self.inner.listen_addr).await?;

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let inner = Arc::clone(&self.inner);

            tokio::spawn(async move {
                let result = if let Some(ref tls_acceptor) = inner.tls_acceptor {
                    // TLS connection
                    match tls_acceptor.accept(stream).await {
                        Ok(tls_stream) => handle_ws_connection(tls_stream, &inner).await,
                        Err(e) => {
                            eprintln!("TLS handshake failed from {}: {}", peer_addr, e);
                            return;
                        }
                    }
                } else {
                    // Plain TCP connection
                    handle_ws_connection(stream, &inner).await
                };

                if let Err(e) = result {
                    eprintln!("Error handling connection from {}: {}", peer_addr, e);
                }
            });
        }
    }
}

async fn handle_ws_connection<S>(stream: S, inner: &ProxyServerInner) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Extract the path from the WebSocket handshake
    let path = Arc::new(std::sync::Mutex::new(String::new()));
    let path_clone = Arc::clone(&path);

    #[allow(clippy::result_large_err)] // the err variant is the error response
    let callback = move |req: &Request, response: Response| {
        let uri_path = req.uri().path().to_string();
        *path_clone.lock().unwrap() = uri_path;
        Ok(response)
    };

    // Accept WebSocket connection with header callback
    let ws_stream = tokio_tungstenite::accept_hdr_async(stream, callback).await?;

    // Get the path and find the target
    let request_path = path.lock().unwrap().clone();
    let target = inner
        .routes
        .get(&request_path)
        .or_else(|| {
            // Try matching without trailing slash
            let normalized = request_path.trim_end_matches('/');
            inner.routes.get(normalized)
        })
        .or(inner.default_target.as_ref())
        .ok_or_else(|| Error::no_route_found(request_path.clone()))?;
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Connect to TCP target (resolve address at connection time)
    let target_addr = target.resolve()?;
    let tcp_stream = TcpStream::connect(target_addr).await?;
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    // Forward WebSocket -> TCP
    let ws_to_tcp = async {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    tcp_write.write_all(&data).await?;
                }
                Ok(Message::Text(text)) => {
                    tcp_write.write_all(text.as_bytes()).await?;
                }
                Ok(Message::Close(_)) => {
                    break;
                }
                Ok(Message::Ping(data)) => {
                    // Respond to ping with pong - handled by the library
                    let _ = data;
                }
                Ok(Message::Pong(_)) => {
                    // Ignore pongs
                }
                Ok(Message::Frame(_)) => {
                    // Raw frames, ignore
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }
        Ok::<_, Error>(())
    };

    // Forward TCP -> WebSocket
    let tcp_to_ws = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = tcp_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ws_write
                .send(Message::Binary(buf[..n].to_vec().into()))
                .await?;
        }
        Ok::<_, Error>(())
    };

    // Run both directions concurrently
    tokio::select! {
        result = ws_to_tcp => result?,
        result = tcp_to_ws => result?,
    }

    Ok(())
}

async fn handle_ws_connection_shared<S>(
    stream: S,
    config: Arc<RwLock<ResolvedConfig>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Extract the path from the WebSocket handshake
    let path = Arc::new(std::sync::Mutex::new(String::new()));
    let path_clone = Arc::clone(&path);

    #[allow(clippy::result_large_err)]
    let callback = move |req: &Request, response: Response| {
        let uri_path = req.uri().path().to_string();
        *path_clone.lock().unwrap() = uri_path;
        Ok(response)
    };

    // Accept WebSocket connection with header callback
    let ws_stream = tokio_tungstenite::accept_hdr_async(stream, callback).await?;

    // Get the path and find the target (using current config)
    let request_path = path.lock().unwrap().clone();
    let target = {
        let cfg = config.read().await;
        cfg.routes
            .get(&request_path)
            .or_else(|| {
                let normalized = request_path.trim_end_matches('/');
                cfg.routes.get(normalized)
            })
            .or(cfg.default_target.as_ref())
            .cloned()
            .ok_or_else(|| Error::no_route_found(request_path.clone()))?
    };

    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Connect to TCP target (resolve address at connection time)
    let target_addr = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::config("could not resolve address"))?;
    let tcp_stream = TcpStream::connect(target_addr).await?;
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    // Forward WebSocket -> TCP
    let ws_to_tcp = async {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    tcp_write.write_all(&data).await?;
                }
                Ok(Message::Text(text)) => {
                    tcp_write.write_all(text.as_bytes()).await?;
                }
                Ok(Message::Close(_)) => {
                    break;
                }
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                Err(e) => {
                    return Err(e.into());
                }
            }
        }
        Ok::<_, Error>(())
    };

    // Forward TCP -> WebSocket
    let tcp_to_ws = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = tcp_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ws_write
                .send(Message::Binary(buf[..n].to_vec().into()))
                .await?;
        }
        Ok::<_, Error>(())
    };

    tokio::select! {
        result = ws_to_tcp => result?,
        result = tcp_to_ws => result?,
    }

    Ok(())
}
