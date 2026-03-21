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
