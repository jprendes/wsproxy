//! WebSocket Proxy Client
//!
//! Listens for TCP connections and forwards data through WebSocket to a server.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

use crate::error::{Error, Result};

/// TLS options for the client
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    /// Skip certificate verification (insecure, for self-signed certificates)
    pub insecure: bool,
    /// Path to CA certificate file (PEM format) for verifying self-signed server certificates
    pub ca_cert_path: Option<String>,
}

/// Run a proxy client with the given configuration.
///
/// This is a convenience function that builds and runs a `ProxyClient`.
///
/// # Arguments
///
/// * `listen` - Address to listen for TCP connections (e.g., "127.0.0.1:2222")
/// * `server_url` - WebSocket server URL to connect to (e.g., "ws://server:8080/ssh")
/// * `tls_options` - TLS options for certificate verification
///
/// # Example
///
/// ```no_run
/// # async fn example() -> wsproxy::Result<()> {
/// wsproxy::client::run("127.0.0.1:2222", "ws://server:8080/ssh", &Default::default()).await?;
/// # Ok(())
/// # }
/// ```
pub async fn run(listen: &str, server_url: &str, tls_options: &TlsOptions) -> Result<()> {
    let client = ProxyClient::bind(listen, server_url, tls_options.clone())?;

    eprintln!(
        "Proxy client listening on {}, forwarding to {}",
        listen, server_url
    );

    client.run().await
}

/// Run a single tunnel connection using stdin/stdout.
///
/// This is useful for SSH ProxyCommand integration. The tunnel connects to
/// the WebSocket server and forwards data between stdin/stdout and the WebSocket.
///
/// # Arguments
///
/// * `server_url` - WebSocket server URL to connect to (e.g., "ws://server:8080/ssh")
/// * `tls_options` - TLS options for certificate verification
///
/// # Example SSH Config
///
/// ```text
/// Host myserver
///   ProxyCommand wsproxy tunnel --server wss://proxy:8080/ssh
///   User myuser
///   HostName localhost
/// ```
pub async fn tunnel(server_url: &str, tls_options: &TlsOptions) -> Result<()> {
    use tokio::io::{stdin, stdout};
    // Connect to WebSocket server
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let request = server_url.into_client_request()?;
    let uri = request.uri();
    let scheme = uri.scheme_str().unwrap_or("ws");
    let host = uri
        .host()
        .ok_or_else(|| Error::config("missing host in URL"))?;
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "wss" { 443 } else { 80 });

    let addr = format!("{}:{}", host, port);
    let tcp_conn = TcpStream::connect(&addr).await?;

    if scheme == "wss" {
        // TLS connection
        use tokio_rustls::rustls::pki_types::ServerName;

        let config = build_tls_config(tls_options)?;

        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = ServerName::try_from(host.to_string())
            .map_err(|e| Error::config(format!("invalid server name: {}", e)))?;

        let tls_stream = connector.connect(server_name, tcp_conn).await?;
        let (ws_stream, _response) = tokio_tungstenite::client_async(request, tls_stream).await?;
        forward_ws_stdio(ws_stream, stdin(), stdout()).await
    } else {
        // Plain TCP connection
        let (ws_stream, _response) = tokio_tungstenite::client_async(request, tcp_conn).await?;
        forward_ws_stdio(ws_stream, stdin(), stdout()).await
    }
}

#[derive(Debug)]
struct ProxyClientInner {
    listen_addr: SocketAddr,
    server_url: String,
    tls_options: TlsOptions,
}

/// A proxy client that forwards TCP connections through WebSocket.
///
/// # Example
///
/// ```no_run
/// use wsproxy::ProxyClient;
///
/// # async fn example() -> wsproxy::Result<()> {
/// let client = ProxyClient::bind(
///     "127.0.0.1:2222",
///     "ws://proxy-server:8080/ssh",
///     Default::default(),
/// )?;
///
/// client.run().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ProxyClient {
    inner: Arc<ProxyClientInner>,
}

impl ProxyClient {
    /// Create a new proxy client.
    ///
    /// # Arguments
    ///
    /// * `listen_addr` - The address to listen for TCP connections.
    /// * `server_url` - The WebSocket server URL to connect to (e.g., "ws://127.0.0.1:8080/ssh").
    /// * `tls_options` - TLS options for certificate verification.
    pub fn bind(
        listen_addr: impl ToSocketAddrs,
        server_url: impl Into<String>,
        tls_options: TlsOptions,
    ) -> Result<Self> {
        let listen_addr = listen_addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| Error::config("could not resolve address"))?;

        Ok(Self {
            inner: Arc::new(ProxyClientInner {
                listen_addr,
                server_url: server_url.into(),
                tls_options,
            }),
        })
    }

    /// Run the proxy client.
    ///
    /// This will listen for TCP connections and forward data through WebSocket to the server.
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(self.inner.listen_addr).await?;

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let server_url = self.inner.server_url.clone();
            let tls_options = self.inner.tls_options.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_tcp_connection(stream, &server_url, &tls_options).await {
                    eprintln!("Error handling connection from {}: {}", peer_addr, e);
                }
            });
        }
    }
}

async fn handle_tcp_connection(
    tcp_stream: TcpStream,
    server_url: &str,
    tls_options: &TlsOptions,
) -> Result<()> {
    // Connect to WebSocket server (supports both ws:// and wss://)
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let request = server_url.into_client_request()?;
    let uri = request.uri();
    let scheme = uri.scheme_str().unwrap_or("ws");
    let host = uri
        .host()
        .ok_or_else(|| Error::config("missing host in URL"))?;
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "wss" { 443 } else { 80 });

    let addr = format!("{}:{}", host, port);
    let tcp_conn = TcpStream::connect(&addr).await?;

    if scheme == "wss" {
        // TLS connection
        use tokio_rustls::rustls::pki_types::ServerName;

        let config = build_tls_config(tls_options)?;

        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = ServerName::try_from(host.to_string())
            .map_err(|e| Error::config(format!("invalid server name: {}", e)))?;

        let tls_stream = connector.connect(server_name, tcp_conn).await?;
        let (ws_stream, _response) = tokio_tungstenite::client_async(request, tls_stream).await?;
        forward_ws_tcp(ws_stream, tcp_stream).await
    } else {
        // Plain TCP connection
        let (ws_stream, _response) = tokio_tungstenite::client_async(request, tcp_conn).await?;
        forward_ws_tcp(ws_stream, tcp_stream).await
    }
}

/// Build TLS client configuration based on options
fn build_tls_config(tls_options: &TlsOptions) -> Result<tokio_rustls::rustls::ClientConfig> {
    use tokio_rustls::rustls::client::danger::{
        HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
    };
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{DigitallySignedStruct, SignatureScheme};

    if tls_options.insecure {
        // Skip certificate verification (dangerous!)
        #[derive(Debug)]
        struct InsecureVerifier;

        impl ServerCertVerifier for InsecureVerifier {
            fn verify_server_cert(
                &self,
                _end_entity: &CertificateDer<'_>,
                _intermediates: &[CertificateDer<'_>],
                _server_name: &ServerName<'_>,
                _ocsp_response: &[u8],
                _now: UnixTime,
            ) -> std::result::Result<ServerCertVerified, tokio_rustls::rustls::Error> {
                Ok(ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> std::result::Result<HandshakeSignatureValid, tokio_rustls::rustls::Error>
            {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> std::result::Result<HandshakeSignatureValid, tokio_rustls::rustls::Error>
            {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                vec![
                    SignatureScheme::RSA_PKCS1_SHA256,
                    SignatureScheme::RSA_PKCS1_SHA384,
                    SignatureScheme::RSA_PKCS1_SHA512,
                    SignatureScheme::ECDSA_NISTP256_SHA256,
                    SignatureScheme::ECDSA_NISTP384_SHA384,
                    SignatureScheme::ECDSA_NISTP521_SHA512,
                    SignatureScheme::RSA_PSS_SHA256,
                    SignatureScheme::RSA_PSS_SHA384,
                    SignatureScheme::RSA_PSS_SHA512,
                    SignatureScheme::ED25519,
                ]
            }
        }

        let config = tokio_rustls::rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureVerifier))
            .with_no_client_auth();

        Ok(config)
    } else if let Some(ca_cert_path) = &tls_options.ca_cert_path {
        // Use custom CA certificate
        use std::io::BufReader;

        let ca_file = std::fs::File::open(ca_cert_path).map_err(|e| {
            Error::config(format!(
                "failed to open CA certificate '{}': {}",
                ca_cert_path, e
            ))
        })?;

        let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(ca_file))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::config(format!("failed to parse CA certificate: {}", e)))?;

        let mut root_store = tokio_rustls::rustls::RootCertStore::empty();
        for cert in certs {
            root_store.add(cert).map_err(|e| {
                Error::config(format!("failed to add CA certificate to root store: {}", e))
            })?;
        }

        let config = tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        Ok(config)
    } else {
        // Use system root certificates
        let mut root_store = tokio_rustls::rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let config = tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        Ok(config)
    }
}

async fn forward_ws_tcp<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    tcp_stream: TcpStream,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Split TCP stream
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

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
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {
                    // Handled by the library or ignored
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }
        Ok::<_, Error>(())
    };

    // Run both directions concurrently
    tokio::select! {
        result = tcp_to_ws => result?,
        result = ws_to_tcp => result?,
    }

    Ok(())
}

async fn forward_ws_stdio<S, R, W>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    mut stdin: R,
    mut stdout: W,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Forward stdin -> WebSocket
    let stdin_to_ws = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = stdin.read(&mut buf).await?;
            if n == 0 {
                // stdin closed, send close frame
                let _ = ws_write.send(Message::Close(None)).await;
                break;
            }
            ws_write
                .send(Message::Binary(buf[..n].to_vec().into()))
                .await?;
        }
        Ok::<_, Error>(())
    };

    // Forward WebSocket -> stdout
    let ws_to_stdout = async {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    stdout.write_all(&data).await?;
                    stdout.flush().await?;
                }
                Ok(Message::Text(text)) => {
                    stdout.write_all(text.as_bytes()).await?;
                    stdout.flush().await?;
                }
                Ok(Message::Close(_)) => {
                    break;
                }
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {
                    // Handled by the library or ignored
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }
        Ok::<_, Error>(())
    };

    // Run both directions concurrently
    tokio::select! {
        result = stdin_to_ws => result?,
        result = ws_to_stdout => result?,
    }

    Ok(())
}
