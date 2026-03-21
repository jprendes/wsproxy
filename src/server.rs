//! WebSocket Proxy Server
//!
//! Listens for WebSocket connections and forwards data to a TCP target.

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;

use crate::error::{Error, Result};

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
///     .default_target("127.0.0.1:22")?
///     .bind("0.0.0.0:8080")?;
///
/// // Server with multiple routes
/// let server = ProxyServer::builder()
///     .route("/ssh", "127.0.0.1:22")?
///     .route("/db", "127.0.0.1:5432")?
///     .route("/redis", "127.0.0.1:6379")?
///     .bind("0.0.0.0:8080")?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct ProxyServerBuilder {
    routes: HashMap<String, SocketAddr>,
    default_target: Option<SocketAddr>,
}

impl ProxyServerBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a route mapping a URL path to a TCP target address.
    pub fn route(mut self, path: impl Into<String>, target: impl ToSocketAddrs) -> Result<Self> {
        let target = resolve_addr(target)?;
        self.routes.insert(path.into(), target);
        Ok(self)
    }

    /// Set the default target for paths that don't match any route.
    pub fn default_target(mut self, target: impl ToSocketAddrs) -> Result<Self> {
        self.default_target = Some(resolve_addr(target)?);
        Ok(self)
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
            return Err(Error::Config(
                "at least one route or a default_target is required".to_string(),
            ));
        }

        Ok(ProxyServer {
            inner: Arc::new(ProxyServerInner {
                listen_addr,
                routes: self.routes,
                default_target: self.default_target,
            }),
        })
    }
}

fn resolve_addr(addr: impl ToSocketAddrs) -> Result<SocketAddr> {
    addr.to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::Config("could not resolve address".to_string()))
}

#[derive(Debug)]
struct ProxyServerInner {
    listen_addr: SocketAddr,
    routes: HashMap<String, SocketAddr>,
    default_target: Option<SocketAddr>,
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
                if let Err(e) = handle_ws_connection(stream, inner).await {
                    eprintln!("Error handling connection from {}: {}", peer_addr, e);
                }
            });
        }
    }
}

async fn handle_ws_connection(stream: TcpStream, inner: Arc<ProxyServerInner>) -> Result<()> {
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
    let target_addr = inner
        .routes
        .get(&request_path)
        .or_else(|| {
            // Try matching without trailing slash
            let normalized = request_path.trim_end_matches('/');
            inner.routes.get(normalized)
        })
        .or(inner.default_target.as_ref())
        .ok_or_else(|| Error::NoRouteFound(request_path.clone()))?;
    let target_addr = *target_addr;
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Connect to TCP target
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
                    return Err(Error::WebSocket(e));
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
