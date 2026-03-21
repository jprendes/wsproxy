//! Integration tests for wsproxy

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wsproxy::{ProxyClient, ProxyServer};

/// Create a simple TCP echo server that echoes back any data it receives.
async fn start_echo_server(addr: &str) -> u16 {
    let listener = TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    stream.write_all(&buf[..n]).await.unwrap();
                }
            });
        }
    });

    port
}

#[tokio::test]
async fn test_roundtrip_tcp_ws_tcp() {
    // 1. Start TCP echo server
    let echo_port = start_echo_server("127.0.0.1:0").await;
    let echo_addr = format!("127.0.0.1:{}", echo_port);

    // 2. Start ProxyServer (WS -> TCP echo server)
    // Get the actual bound address by running the server
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    let proxy_server = ProxyServer::builder()
        .default_target(&echo_addr)
        .unwrap()
        .bind(format!("127.0.0.1:{}", ws_port))
        .unwrap();

    tokio::spawn(async move {
        proxy_server.run().await.unwrap();
    });

    // Give the server time to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Start ProxyClient (TCP -> WS)
    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();
    drop(client_listener);

    let proxy_client = ProxyClient::bind(
        format!("127.0.0.1:{}", client_port),
        format!("ws://127.0.0.1:{}", ws_port),
    )
    .unwrap();

    tokio::spawn(async move {
        proxy_client.run().await.unwrap();
    });

    // Give the client time to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 4. Connect TCP client to ProxyClient and test roundtrip
    let mut client = TcpStream::connect(format!("127.0.0.1:{}", client_port))
        .await
        .unwrap();

    // Send test data
    let test_data = b"Hello, WebSocket proxy!";
    client.write_all(test_data).await.unwrap();

    // Read response
    let mut response = vec![0u8; test_data.len()];
    client.read_exact(&mut response).await.unwrap();

    assert_eq!(response, test_data);

    // Test multiple roundtrips
    for i in 0..5 {
        let msg = format!("Message {}", i);
        client.write_all(msg.as_bytes()).await.unwrap();

        let mut response = vec![0u8; msg.len()];
        client.read_exact(&mut response).await.unwrap();

        assert_eq!(response, msg.as_bytes());
    }
}

#[tokio::test]
async fn test_multiple_routes() {
    // Start two different echo servers
    let echo1_port = start_echo_server("127.0.0.1:0").await;
    let echo2_port = start_echo_server("127.0.0.1:0").await;

    // Start ProxyServer with routes
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    let proxy_server = ProxyServer::builder()
        .route("/echo1", format!("127.0.0.1:{}", echo1_port))
        .unwrap()
        .route("/echo2", format!("127.0.0.1:{}", echo2_port))
        .unwrap()
        .bind(format!("127.0.0.1:{}", ws_port))
        .unwrap();

    tokio::spawn(async move {
        proxy_server.run().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Start two ProxyClients for different routes
    let client1_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client1_port = client1_listener.local_addr().unwrap().port();
    drop(client1_listener);

    let proxy_client1 = ProxyClient::bind(
        format!("127.0.0.1:{}", client1_port),
        format!("ws://127.0.0.1:{}/echo1", ws_port),
    )
    .unwrap();

    let client2_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client2_port = client2_listener.local_addr().unwrap().port();
    drop(client2_listener);

    let proxy_client2 = ProxyClient::bind(
        format!("127.0.0.1:{}", client2_port),
        format!("ws://127.0.0.1:{}/echo2", ws_port),
    )
    .unwrap();

    tokio::spawn(async move {
        proxy_client1.run().await.unwrap();
    });

    tokio::spawn(async move {
        proxy_client2.run().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Test both routes work independently
    let mut client1 = TcpStream::connect(format!("127.0.0.1:{}", client1_port))
        .await
        .unwrap();
    let mut client2 = TcpStream::connect(format!("127.0.0.1:{}", client2_port))
        .await
        .unwrap();

    let msg1 = b"Route 1 message";
    let msg2 = b"Route 2 message";

    client1.write_all(msg1).await.unwrap();
    client2.write_all(msg2).await.unwrap();

    let mut response1 = vec![0u8; msg1.len()];
    let mut response2 = vec![0u8; msg2.len()];

    client1.read_exact(&mut response1).await.unwrap();
    client2.read_exact(&mut response2).await.unwrap();

    assert_eq!(response1, msg1);
    assert_eq!(response2, msg2);
}

#[tokio::test]
async fn test_large_data_transfer() {
    let echo_port = start_echo_server("127.0.0.1:0").await;

    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    let proxy_server = ProxyServer::builder()
        .default_target(format!("127.0.0.1:{}", echo_port))
        .unwrap()
        .bind(format!("127.0.0.1:{}", ws_port))
        .unwrap();

    tokio::spawn(async move {
        proxy_server.run().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();
    drop(client_listener);

    let proxy_client = ProxyClient::bind(
        format!("127.0.0.1:{}", client_port),
        format!("ws://127.0.0.1:{}", ws_port),
    )
    .unwrap();

    tokio::spawn(async move {
        proxy_client.run().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(format!("127.0.0.1:{}", client_port))
        .await
        .unwrap();

    // Send 1MB of data
    let large_data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
    client.write_all(&large_data).await.unwrap();

    // Read it back
    let mut response = vec![0u8; large_data.len()];
    client.read_exact(&mut response).await.unwrap();

    assert_eq!(response, large_data);
}

const REGISTRY_FILE_ENV: &str = "WSPROXY_REGISTRY_FILE";
const WSPROXY_BIN: &str = env!("CARGO_BIN_EXE_wsproxy");

/// RAII guard that cleans up daemons on drop
struct DaemonRunner {
    registry_dir: tempfile::TempDir,
}

impl DaemonRunner {
    fn new() -> Self {
        Self {
            registry_dir: tempfile::tempdir().unwrap(),
        }
    }

    fn registry_file(&self) -> std::path::PathBuf {
        self.registry_dir.path().join("daemons.json")
    }

    fn spawn_server(&self, args: &[&str]) -> std::process::ExitStatus {
        use std::process::{Command, Stdio};

        let mut full_args = vec!["daemon", "server"];
        full_args.extend(args);

        Command::new(WSPROXY_BIN)
            .args(&full_args)
            .env(REGISTRY_FILE_ENV, self.registry_file())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to start server daemon")
    }

    fn spawn_client(&self, args: &[&str]) -> std::process::ExitStatus {
        use std::process::{Command, Stdio};

        let mut full_args = vec!["daemon", "client"];
        full_args.extend(args);

        Command::new(WSPROXY_BIN)
            .args(&full_args)
            .env(REGISTRY_FILE_ENV, self.registry_file())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to start client daemon")
    }
}

impl Drop for DaemonRunner {
    fn drop(&mut self) {
        use std::process::Command;

        let registry_file = self.registry_file();
        let output = Command::new(WSPROXY_BIN)
            .args(["daemon", "list"])
            .env(REGISTRY_FILE_ENV, &registry_file)
            .output()
            .expect("Failed to list daemons");

        let list_output = String::from_utf8_lossy(&output.stdout);
        for line in list_output.lines().skip(2) {
            if let Some(id) = line.split_whitespace().next() {
                if id.parse::<u32>().is_ok() {
                    Command::new(WSPROXY_BIN)
                        .args(["daemon", "kill", id])
                        .env(REGISTRY_FILE_ENV, &registry_file)
                        .output()
                        .ok();
                }
            }
        }
        // tempfile::TempDir handles file cleanup automatically
    }
}

/// Test bidirectional "chat" communication through the proxy using daemon mode.
/// This mimics the netcat example from the README where two parties
/// can send messages to each other through the WebSocket proxy.
#[tokio::test]
async fn test_bidirectional_chat_daemon() {
    let runner = DaemonRunner::new();

    // 1. Start a TCP listener (the "chat server" - like `nc -l 9000`)
    let chat_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let chat_server_port = chat_server.local_addr().unwrap().port();

    // 2. Find available ports for proxy server and client
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();
    drop(client_listener);

    // 3. Start the proxy server daemon
    let status = runner.spawn_server(&[
        "--listen",
        &format!("127.0.0.1:{}", ws_port),
        "--default-target",
        &format!("127.0.0.1:{}", chat_server_port),
    ]);
    assert!(status.success(), "Server daemon failed");

    // 4. Start the proxy client daemon
    let status = runner.spawn_client(&[
        "--listen",
        &format!("127.0.0.1:{}", client_port),
        "--server",
        &format!("ws://127.0.0.1:{}", ws_port),
    ]);
    assert!(status.success(), "Client daemon failed");

    // Give daemons time to fully start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 5. Connect to the proxy and accept on chat server SIMULTANEOUSLY
    // The connect() blocks until the full chain is established, which requires
    // the chat server to accept the incoming connection from the proxy server.
    let (connect_result, accept_result) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect(format!("127.0.0.1:{}", client_port)),
        ),
        tokio::time::timeout(Duration::from_secs(5), chat_server.accept()),
    );

    let mut chat_client = connect_result
        .expect("Timeout connecting to proxy client")
        .expect("Failed to connect to proxy client");

    let (mut chat_server_conn, _) = accept_result
        .expect("Timeout waiting for connection on chat server")
        .expect("Failed to accept connection");

    // Test bidirectional communication

    // Client sends to server
    let client_msg = b"Hello from client!";
    chat_client.write_all(client_msg).await.unwrap();

    let mut server_received = vec![0u8; client_msg.len()];
    chat_server_conn
        .read_exact(&mut server_received)
        .await
        .unwrap();
    assert_eq!(server_received, client_msg);

    // Server sends to client
    let server_msg = b"Hello from server!";
    chat_server_conn.write_all(server_msg).await.unwrap();

    let mut client_received = vec![0u8; server_msg.len()];
    chat_client.read_exact(&mut client_received).await.unwrap();
    assert_eq!(client_received, server_msg);

    // Multiple back-and-forth messages (like a real chat)
    for i in 0..5 {
        // Client -> Server
        let msg = format!("Client message {}\n", i);
        chat_client.write_all(msg.as_bytes()).await.unwrap();

        let mut received = vec![0u8; msg.len()];
        chat_server_conn.read_exact(&mut received).await.unwrap();
        assert_eq!(received, msg.as_bytes());

        // Server -> Client
        let reply = format!("Server reply {}\n", i);
        chat_server_conn.write_all(reply.as_bytes()).await.unwrap();

        let mut received = vec![0u8; reply.len()];
        chat_client.read_exact(&mut received).await.unwrap();
        assert_eq!(received, reply.as_bytes());
    }

    // runner dropped here, cleans up daemons automatically
}
