//! WebSocket Proxy Library
//!
//! This library provides a `ProxyServer` and `ProxyClient` for proxying
//! TCP connections through WebSocket.
//!
//! # Architecture
//!
//! ```text
//! TCP Client <---> ProxyClient <--WebSocket--> ProxyServer <---> TCP Server
//! ```
//!
//! ## ProxyServer
//!
//! The server listens for WebSocket connections and forwards data to a TCP target.
//! Each WebSocket connection results in a new TCP connection to the target.
//!
//! ## ProxyClient
//!
//! The client listens for TCP connections and forwards data through WebSocket
//! to the server. Each TCP connection results in a new WebSocket connection.
//!
//! # Example
//!
//! ```no_run
//! use wsproxy::{ProxyServer, ProxyClient, TlsOptions};
//!
//! #[tokio::main]
//! async fn main() -> wsproxy::Result<()> {
//!     // Server with a single default target
//!     let server = ProxyServer::builder()
//!         .default_target("127.0.0.1:22")?
//!         .bind("0.0.0.0:8080")?;
//!
//!     // Server with multiple routes (different endpoints -> different TCP targets)
//!     let server = ProxyServer::builder()
//!         .route("/ssh", "127.0.0.1:22")?
//!         .route("/db", "127.0.0.1:5432")?
//!         .route("/redis", "127.0.0.1:6379")?
//!         .bind("0.0.0.0:8080")?;
//!
//!     // Client: listen for TCP on port 2222, forward to WebSocket server
//!     let client = ProxyClient::bind(
//!         "127.0.0.1:2222",
//!         "ws://proxy-server:8080/ssh",
//!         TlsOptions::default(),
//!     )?;
//!
//!     // Run server or client (typically in separate processes)
//!     // server.run().await?;
//!     // client.run().await?;
//!     Ok(())
//! }
//! ```

pub mod client;
mod error;
pub mod server;

pub use client::{ProxyClient, TlsOptions};
pub use error::{Error, Result};
pub use server::{ProxyServer, ProxyServerBuilder, TlsMode};
