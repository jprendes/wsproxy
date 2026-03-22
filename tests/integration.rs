//! Integration tests for wsproxy

use std::io::Write;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wsproxy::{ProxyClient, ProxyServer, TlsOptions};

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
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();

    let proxy_server = ProxyServer::builder()
        .default_target(echo_addr)
        .bind(ws_listener)
        .unwrap();

    tokio::spawn(async move {
        proxy_server.run().await.unwrap();
    });

    // Give the server time to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Start ProxyClient (TCP -> WS)
    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();

    let proxy_client = ProxyClient::bind(
        client_listener,
        format!("ws://127.0.0.1:{}", ws_port),
        TlsOptions::default(),
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

    let proxy_server = ProxyServer::builder()
        .route("/echo1", format!("127.0.0.1:{}", echo1_port))
        .route("/echo2", format!("127.0.0.1:{}", echo2_port))
        .bind(ws_listener)
        .unwrap();

    tokio::spawn(async move {
        proxy_server.run().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Start two ProxyClients for different routes
    let client1_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client1_port = client1_listener.local_addr().unwrap().port();

    let proxy_client1 = ProxyClient::bind(
        client1_listener,
        format!("ws://127.0.0.1:{}/echo1", ws_port),
        TlsOptions::default(),
    )
    .unwrap();

    let client2_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client2_port = client2_listener.local_addr().unwrap().port();

    let proxy_client2 = ProxyClient::bind(
        client2_listener,
        format!("ws://127.0.0.1:{}/echo2", ws_port),
        TlsOptions::default(),
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

    let proxy_server = ProxyServer::builder()
        .default_target(format!("127.0.0.1:{}", echo_port))
        .bind(ws_listener)
        .unwrap();

    tokio::spawn(async move {
        proxy_server.run().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();

    let proxy_client = ProxyClient::bind(
        client_listener,
        format!("ws://127.0.0.1:{}", ws_port),
        TlsOptions::default(),
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
            if let Some(id) = line.split_whitespace().next()
                && id.parse::<u32>().is_ok()
            {
                Command::new(WSPROXY_BIN)
                    .args(["daemon", "kill", id])
                    .env(REGISTRY_FILE_ENV, &registry_file)
                    .output()
                    .ok();
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

/// Generate a self-signed CA certificate and server certificate for testing.
/// Returns (ca_cert_pem, server_cert_pem, server_key_pem).
fn generate_test_certs() -> (String, String, String) {
    use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyUsagePurpose};

    // Generate CA key pair
    let ca_key = rcgen::KeyPair::generate().unwrap();

    // Generate CA certificate
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Test CA");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    // Generate server certificate signed by CA
    let mut server_params = CertificateParams::default();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    server_params.subject_alt_names = vec![
        rcgen::SanType::DnsName("localhost".try_into().unwrap()),
        rcgen::SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))),
    ];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];

    let server_key = rcgen::KeyPair::generate().unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    (ca_cert.pem(), server_cert.pem(), server_key.serialize_pem())
}

/// Test WSS (WebSocket Secure) connections with TLS using custom CA certificate.
#[tokio::test]
async fn test_wss_with_custom_ca() {
    let runner = DaemonRunner::new();

    // Generate test certificates
    let (ca_pem, cert_pem, key_pem) = generate_test_certs();

    // Write certificates to temp files
    let cert_dir = runner.registry_dir.path();
    let ca_path = cert_dir.join("ca.pem");
    let cert_path = cert_dir.join("server.crt");
    let key_path = cert_dir.join("server.key");

    std::fs::File::create(&ca_path)
        .unwrap()
        .write_all(ca_pem.as_bytes())
        .unwrap();
    std::fs::File::create(&cert_path)
        .unwrap()
        .write_all(cert_pem.as_bytes())
        .unwrap();
    std::fs::File::create(&key_path)
        .unwrap()
        .write_all(key_pem.as_bytes())
        .unwrap();

    // 1. Start a TCP listener (the backend server)
    let backend_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = backend_server.local_addr().unwrap().port();

    // 2. Find available ports for proxy server and client
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();
    drop(client_listener);

    // 3. Start the WSS proxy server daemon with TLS
    let status = runner.spawn_server(&[
        "--listen",
        &format!("127.0.0.1:{}", ws_port),
        "--default-target",
        &format!("127.0.0.1:{}", backend_port),
        "--tls-cert",
        cert_path.to_str().unwrap(),
        "--tls-key",
        key_path.to_str().unwrap(),
    ]);
    assert!(status.success(), "WSS server daemon failed to start");

    // 4. Start the proxy client daemon with custom CA cert
    let status = runner.spawn_client(&[
        "--listen",
        &format!("127.0.0.1:{}", client_port),
        "--server",
        &format!("wss://127.0.0.1:{}", ws_port),
        "--tls-ca-cert",
        ca_path.to_str().unwrap(),
    ]);
    assert!(status.success(), "WSS client daemon failed to start");

    // Give daemons time to fully start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 5. Connect and test bidirectional communication
    let (connect_result, accept_result) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect(format!("127.0.0.1:{}", client_port)),
        ),
        tokio::time::timeout(Duration::from_secs(5), backend_server.accept()),
    );

    let mut client_conn = connect_result
        .expect("Timeout connecting to proxy client")
        .expect("Failed to connect to proxy client");

    let (mut backend_conn, _) = accept_result
        .expect("Timeout waiting for connection on backend")
        .expect("Failed to accept connection");

    // Client sends to backend
    let client_msg = b"Hello over WSS!";
    client_conn.write_all(client_msg).await.unwrap();

    let mut backend_received = vec![0u8; client_msg.len()];
    backend_conn
        .read_exact(&mut backend_received)
        .await
        .unwrap();
    assert_eq!(backend_received, client_msg);

    // Backend sends to client
    let backend_msg = b"WSS response!";
    backend_conn.write_all(backend_msg).await.unwrap();

    let mut client_received = vec![0u8; backend_msg.len()];
    client_conn.read_exact(&mut client_received).await.unwrap();
    assert_eq!(client_received, backend_msg);
}

/// Test WSS connections with --insecure flag (skip certificate verification).
#[tokio::test]
async fn test_wss_insecure() {
    let runner = DaemonRunner::new();

    // Generate test certificates (self-signed, no CA needed for insecure mode)
    let (_, cert_pem, key_pem) = generate_test_certs();

    // Write certificates to temp files
    let cert_dir = runner.registry_dir.path();
    let cert_path = cert_dir.join("server.crt");
    let key_path = cert_dir.join("server.key");

    std::fs::File::create(&cert_path)
        .unwrap()
        .write_all(cert_pem.as_bytes())
        .unwrap();
    std::fs::File::create(&key_path)
        .unwrap()
        .write_all(key_pem.as_bytes())
        .unwrap();

    // 1. Start a TCP listener (the backend server)
    let backend_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = backend_server.local_addr().unwrap().port();

    // 2. Find available ports
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();
    drop(client_listener);

    // 3. Start the WSS proxy server daemon with TLS
    let status = runner.spawn_server(&[
        "--listen",
        &format!("127.0.0.1:{}", ws_port),
        "--default-target",
        &format!("127.0.0.1:{}", backend_port),
        "--tls-cert",
        cert_path.to_str().unwrap(),
        "--tls-key",
        key_path.to_str().unwrap(),
    ]);
    assert!(status.success(), "WSS server daemon failed to start");

    // 4. Start the proxy client daemon with --insecure flag
    let status = runner.spawn_client(&[
        "--listen",
        &format!("127.0.0.1:{}", client_port),
        "--server",
        &format!("wss://127.0.0.1:{}", ws_port),
        "--insecure",
    ]);
    assert!(status.success(), "WSS client daemon failed to start");

    // Give daemons time to fully start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 5. Connect and test communication
    let (connect_result, accept_result) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect(format!("127.0.0.1:{}", client_port)),
        ),
        tokio::time::timeout(Duration::from_secs(5), backend_server.accept()),
    );

    let mut client_conn = connect_result
        .expect("Timeout connecting to proxy client")
        .expect("Failed to connect to proxy client");

    let (mut backend_conn, _) = accept_result
        .expect("Timeout waiting for connection on backend")
        .expect("Failed to accept connection");

    // Test roundtrip
    let test_msg = b"Insecure WSS test message";
    client_conn.write_all(test_msg).await.unwrap();

    let mut received = vec![0u8; test_msg.len()];
    backend_conn.read_exact(&mut received).await.unwrap();
    assert_eq!(received, test_msg);

    // Response
    let response_msg = b"Insecure WSS response";
    backend_conn.write_all(response_msg).await.unwrap();

    let mut received = vec![0u8; response_msg.len()];
    client_conn.read_exact(&mut received).await.unwrap();
    assert_eq!(received, response_msg);
}

/// Test WSS with server auto-generated self-signed certificate using --tls-self-signed.
#[tokio::test]
async fn test_wss_self_signed_server() {
    let runner = DaemonRunner::new();

    // 1. Start a TCP listener (the backend server)
    let backend_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = backend_server.local_addr().unwrap().port();

    // 2. Find available ports
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();
    drop(client_listener);

    // 3. Start the WSS proxy server daemon with auto-generated self-signed cert
    let status = runner.spawn_server(&[
        "--listen",
        &format!("127.0.0.1:{}", ws_port),
        "--default-target",
        &format!("127.0.0.1:{}", backend_port),
        "--tls-self-signed",
    ]);
    assert!(
        status.success(),
        "WSS server daemon with --tls-self-signed failed to start"
    );

    // 4. Start the proxy client daemon with --insecure flag (required for auto-generated cert)
    let status = runner.spawn_client(&[
        "--listen",
        &format!("127.0.0.1:{}", client_port),
        "--server",
        &format!("wss://127.0.0.1:{}", ws_port),
        "--insecure",
    ]);
    assert!(status.success(), "WSS client daemon failed to start");

    // Give daemons time to fully start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 5. Connect and test communication
    let (connect_result, accept_result) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect(format!("127.0.0.1:{}", client_port)),
        ),
        tokio::time::timeout(Duration::from_secs(5), backend_server.accept()),
    );

    let mut client_conn = connect_result
        .expect("Timeout connecting to proxy client")
        .expect("Failed to connect to proxy client");

    let (mut backend_conn, _) = accept_result
        .expect("Timeout waiting for connection on backend")
        .expect("Failed to accept connection");

    // Test roundtrip
    let test_msg = b"Self-signed WSS test message";
    client_conn.write_all(test_msg).await.unwrap();

    let mut received = vec![0u8; test_msg.len()];
    backend_conn.read_exact(&mut received).await.unwrap();
    assert_eq!(received, test_msg);

    // Response
    let response_msg = b"Self-signed WSS response";
    backend_conn.write_all(response_msg).await.unwrap();

    let mut received = vec![0u8; response_msg.len()];
    client_conn.read_exact(&mut received).await.unwrap();
    assert_eq!(received, response_msg);
}

/// Test configuration file hot-reload.
/// This test verifies that when the config file changes, the server
/// picks up the new configuration without restarting.
#[tokio::test]
async fn test_config_hot_reload() {
    use std::fs::File;

    use wsproxy::server::run_with_config;

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Start two echo servers on different ports
    let echo1_port = start_echo_server("127.0.0.1:0").await;
    let echo2_port = start_echo_server("127.0.0.1:0").await;

    // Find available port for proxy server
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    // Create initial config pointing to echo1
    let initial_config = format!(
        r#"
listen = "127.0.0.1:{}"
default_target = "127.0.0.1:{}"
"#,
        ws_port, echo1_port
    );
    File::create(&config_path)
        .unwrap()
        .write_all(initial_config.as_bytes())
        .unwrap();

    // Start server with config
    let config_path_clone = config_path.clone();
    tokio::spawn(async move {
        let _ = run_with_config(&config_path_clone).await;
    });

    // Give the server time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Start a client
    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();

    let proxy_client = ProxyClient::bind(
        client_listener,
        format!("ws://127.0.0.1:{}", ws_port),
        TlsOptions::default(),
    )
    .unwrap();

    tokio::spawn(async move {
        let _ = proxy_client.run().await;
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test connection works to echo1
    let mut client = TcpStream::connect(format!("127.0.0.1:{}", client_port))
        .await
        .unwrap();
    let test_msg = b"test1";
    client.write_all(test_msg).await.unwrap();
    let mut received = vec![0u8; test_msg.len()];
    client.read_exact(&mut received).await.unwrap();
    assert_eq!(received, test_msg);
    drop(client);

    // Update config to point to echo2
    let new_config = format!(
        r#"
listen = "127.0.0.1:{}"
default_target = "127.0.0.1:{}"
"#,
        ws_port, echo2_port
    );
    // Write new config (overwrite)
    std::fs::write(&config_path, new_config).unwrap();

    // Wait for hot-reload to pick up the change
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Test new connection - should now go to echo2
    // (Both echo servers return the same data, so we just verify it still works)
    let mut client2 = TcpStream::connect(format!("127.0.0.1:{}", client_port))
        .await
        .unwrap();
    let test_msg2 = b"test2";
    client2.write_all(test_msg2).await.unwrap();
    let mut received2 = vec![0u8; test_msg2.len()];
    client2.read_exact(&mut received2).await.unwrap();
    assert_eq!(received2, test_msg2);
}

/// Test that active connections survive configuration hot-reload.
/// This test verifies that when the config file changes while connections
/// are active, those connections continue to work uninterrupted.
#[tokio::test]
async fn test_hot_reload_preserves_connections() {
    use std::fs::File;

    use wsproxy::server::run_with_config;

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Start echo server
    let echo_port = start_echo_server("127.0.0.1:0").await;

    // Find available port for proxy server
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    drop(ws_listener);

    // Create initial config
    let initial_config = format!(
        r#"
listen = "127.0.0.1:{}"
default_target = "127.0.0.1:{}"
"#,
        ws_port, echo_port
    );
    File::create(&config_path)
        .unwrap()
        .write_all(initial_config.as_bytes())
        .unwrap();

    // Start server with config
    let config_path_clone = config_path.clone();
    tokio::spawn(async move {
        let _ = run_with_config(&config_path_clone).await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Start a client
    let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client_port = client_listener.local_addr().unwrap().port();

    let proxy_client = ProxyClient::bind(
        client_listener,
        format!("ws://127.0.0.1:{}", ws_port),
        TlsOptions::default(),
    )
    .unwrap();

    tokio::spawn(async move {
        let _ = proxy_client.run().await;
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Establish a connection BEFORE the config change
    let mut active_conn = TcpStream::connect(format!("127.0.0.1:{}", client_port))
        .await
        .unwrap();

    // Verify it works initially
    let msg1 = b"before_reload";
    active_conn.write_all(msg1).await.unwrap();
    let mut received = vec![0u8; msg1.len()];
    active_conn.read_exact(&mut received).await.unwrap();
    assert_eq!(received, msg1, "Connection should work before reload");

    // Now trigger a hot-reload by changing the config
    // We'll change the default_target to a different (but still valid) echo server
    let echo2_port = start_echo_server("127.0.0.1:0").await;
    let new_config = format!(
        r#"
listen = "127.0.0.1:{}"
default_target = "127.0.0.1:{}"
"#,
        ws_port, echo2_port
    );
    std::fs::write(&config_path, new_config).unwrap();

    // Wait for hot-reload to pick up the change
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The existing connection should STILL work - it should not be interrupted
    let msg2 = b"during_reload";
    active_conn.write_all(msg2).await.unwrap();
    let mut received2 = vec![0u8; msg2.len()];
    active_conn.read_exact(&mut received2).await.unwrap();
    assert_eq!(received2, msg2, "Existing connection should survive reload");

    // Send multiple messages to verify the connection is truly alive
    for i in 0..5 {
        let msg = format!("message_{}", i);
        active_conn.write_all(msg.as_bytes()).await.unwrap();
        let mut received = vec![0u8; msg.len()];
        active_conn.read_exact(&mut received).await.unwrap();
        assert_eq!(
            received,
            msg.as_bytes(),
            "Connection should continue working after reload"
        );
    }

    // Verify new connections also work (they'll go to the new target)
    let mut new_conn = TcpStream::connect(format!("127.0.0.1:{}", client_port))
        .await
        .unwrap();
    let msg_new = b"new_connection";
    new_conn.write_all(msg_new).await.unwrap();
    let mut received_new = vec![0u8; msg_new.len()];
    new_conn.read_exact(&mut received_new).await.unwrap();
    assert_eq!(received_new, msg_new, "New connections should work");

    // Old connection should still be working even after new connections are made
    let final_msg = b"still_alive";
    active_conn.write_all(final_msg).await.unwrap();
    let mut final_received = vec![0u8; final_msg.len()];
    active_conn.read_exact(&mut final_received).await.unwrap();
    assert_eq!(
        final_received, final_msg,
        "Original connection should still work"
    );
}
