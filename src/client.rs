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

/// Run a proxy client with the given configuration.
///
/// This is a convenience function that builds and runs a `ProxyClient`.
///
/// # Arguments
///
/// * `listen` - Address to listen for TCP connections (e.g., "127.0.0.1:2222")
/// * `server_url` - WebSocket server URL to connect to (e.g., "ws://server:8080/ssh")
///
/// # Example
///
/// ```no_run
/// # async fn example() -> wsproxy::Result<()> {
/// wsproxy::client::run("127.0.0.1:2222", "ws://server:8080/ssh").await?;
/// # Ok(())
/// # }
/// ```
pub async fn run(listen: &str, server_url: &str) -> Result<()> {
    let client = ProxyClient::bind(listen, server_url)?;

    eprintln!(
        "Proxy client listening on {}, forwarding to {}",
        listen, server_url
    );

    client.run().await
}

#[derive(Debug)]
struct ProxyClientInner {
    listen_addr: SocketAddr,
    server_url: String,
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
    pub fn bind(listen_addr: impl ToSocketAddrs, server_url: impl Into<String>) -> Result<Self> {
        let listen_addr = listen_addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| Error::config("could not resolve address"))?;

        Ok(Self {
            inner: Arc::new(ProxyClientInner {
                listen_addr,
                server_url: server_url.into(),
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

            tokio::spawn(async move {
                if let Err(e) = handle_tcp_connection(stream, &server_url).await {
                    eprintln!("Error handling connection from {}: {}", peer_addr, e);
                }
            });
        }
    }
}

async fn handle_tcp_connection(tcp_stream: TcpStream, server_url: &str) -> Result<()> {
    // Connect to WebSocket server
    let (ws_stream, _response) = tokio_tungstenite::connect_async(server_url).await?;
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
