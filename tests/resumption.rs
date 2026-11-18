use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use wsproxy::{ProxyClient, ProxyServer, TlsOptions};

struct Tunnel {
    port: u16,
    url: String,
    online: watch::Sender<bool>,
    transferred: Arc<AtomicUsize>,
    backend_connections: Arc<AtomicUsize>,
    _tasks: JoinSet<Result<()>>,
    config: Option<tempfile::TempDir>,
}

async fn relay<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut reader: R,
    mut writer: W,
    transferred: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            return writer.shutdown().await;
        }
        writer.write_all(&buffer[..n]).await?;
        transferred.fetch_add(n, Ordering::SeqCst);
        sleep(Duration::from_millis(1)).await;
    }
}

impl Tunnel {
    async fn start(window: u32, tls: bool, resumable: Option<bool>) -> Result<Self> {
        let mut tasks: JoinSet<Result<()>> = JoinSet::new();
        let backend = TcpListener::bind("127.0.0.1:0").await?;
        let backend_addr = backend.local_addr()?;
        let backend_connections = Arc::new(AtomicUsize::new(0));
        let count = backend_connections.clone();
        tasks.spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let (stream, _) = backend.accept().await?;
                count.fetch_add(1, Ordering::SeqCst);
                connections.spawn(async move {
                    let (mut reader, mut writer) = stream.into_split();
                    tokio::io::copy(&mut reader, &mut writer).await?;
                    writer.shutdown().await
                });
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let server_addr = listener.local_addr()?;
        let config = if tls {
            let server = ProxyServer::builder()
                .default_target(backend_addr.to_string())
                .reconnect_window_secs(window)
                .tls_self_signed()
                .bind(listener)?;
            tasks.spawn(async move { Ok(server.run().await?) });
            None
        } else {
            let config = tempfile::tempdir()?;
            let path = config.path().join("server.toml");
            std::fs::write(
                &path,
                format!(
                    "listen = '{server_addr}'\ndefault_target = '{backend_addr}'\nreconnect_window_secs = {window}\n",
                ),
            )?;
            drop(listener);
            tasks.spawn(async move {
                Ok(wsproxy::server::run_with_config_until_shutdown(
                    path,
                    std::future::pending(),
                    Duration::from_secs(1),
                )
                .await?)
            });
            timeout(Duration::from_secs(3), async {
                loop {
                    if TcpStream::connect(server_addr).await.is_ok() {
                        break;
                    }
                    sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            Some(config)
        };

        let relay_listener = TcpListener::bind("127.0.0.1:0").await?;
        let relay_addr = relay_listener.local_addr()?;
        let (online, mut status) = watch::channel(true);
        let transferred = Arc::new(AtomicUsize::new(0));
        let progress = transferred.clone();
        tasks.spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let (mut local, _) = relay_listener.accept().await?;
                if !*status.borrow_and_update() {
                    continue;
                }
                let mut status = status.clone();
                let progress = progress.clone();
                connections.spawn(async move {
                    let mut remote = TcpStream::connect(server_addr).await?;
                    let (lr, lw) = local.split();
                    let (rr, rw) = remote.split();
                    tokio::select! {
                        result = async {
                            tokio::try_join!(
                                relay(lr, rw, progress.clone()),
                                relay(rr, lw, progress),
                            )
                        } => { result?; }
                        _ = status.changed() => {}
                    }
                    Ok::<_, std::io::Error>(())
                });
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let url = format!("{}://{relay_addr}/ssh", if tls { "wss" } else { "ws" });
        let client = ProxyClient::bind(
            listener,
            &url,
            TlsOptions {
                insecure: tls,
                ca_cert_path: None,
            },
        )?;
        let client = match resumable {
            Some(required) => client.with_resumption(required),
            None => client,
        };
        tasks.spawn(async move { Ok(client.run().await?) });

        Ok(Self {
            port,
            url,
            online,
            transferred,
            backend_connections,
            _tasks: tasks,
            config,
        })
    }

    async fn connect(&self) -> Result<TcpStream> {
        Ok(TcpStream::connect(("127.0.0.1", self.port)).await?)
    }
}

#[tokio::test]
async fn reconnects_during_bidirectional_transfer_without_replacing_backend() -> Result<()> {
    timeout(Duration::from_secs(30), async {
        for tls in [false, true] {
            let tunnel = Tunnel::start(5, tls, None).await?;
            let stream = tunnel.connect().await?;
            let (mut reader, mut writer) = stream.into_split();
            let data: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
            let expected = data.clone();
            let transfer = async {
                let write = async {
                    writer.write_all(&data).await?;
                    writer.shutdown().await
                };
                let read = async {
                    let mut received = Vec::new();
                    reader.read_to_end(&mut received).await?;
                    Ok::<_, std::io::Error>(received)
                };
                let (_, received) = tokio::try_join!(write, read)?;
                assert_eq!(received.len(), expected.len());
                assert!(received == expected, "recovered stream contents differ");
                Ok::<_, anyhow::Error>(())
            };
            let interruptions = async {
                for _ in 0..3 {
                    let baseline = tunnel.transferred.load(Ordering::SeqCst);
                    while tunnel.transferred.load(Ordering::SeqCst) < baseline + 32768 {
                        sleep(Duration::from_millis(1)).await;
                    }
                    tunnel.online.send(false)?;
                    sleep(Duration::from_millis(100)).await;
                    tunnel.online.send(true)?;
                }
                Ok::<_, anyhow::Error>(())
            };
            tokio::try_join!(transfer, interruptions)?;
            assert_eq!(tunnel.backend_connections.load(Ordering::SeqCst), 1);
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn expires_when_connection_does_not_return() -> Result<()> {
    timeout(Duration::from_secs(8), async {
        let tunnel = Tunnel::start(1, false, None).await?;
        let mut stream = tunnel.connect().await?;
        stream.write_all(b"before").await?;
        let mut before = [0; 6];
        stream.read_exact(&mut before).await?;
        assert_eq!(&before, b"before");
        tunnel.online.send(false)?;
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await?;
        assert!(rest.is_empty());
        assert_eq!(tunnel.backend_connections.load(Ordering::SeqCst), 1);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn raw_clients_and_disabled_servers_use_legacy_forwarding() -> Result<()> {
    timeout(Duration::from_secs(5), async {
        let tunnel = Tunnel::start(5, false, Some(false)).await?;
        let mut stream = tunnel.connect().await?;
        stream.write_all(b"raw").await?;
        let mut response = [0; 3];
        stream.read_exact(&mut response).await?;
        assert_eq!(&response, b"raw");

        let automatic = Tunnel::start(0, false, None).await?;
        let mut stream = automatic.connect().await?;
        stream.write_all(b"raw").await?;
        stream.read_exact(&mut response).await?;
        assert_eq!(&response, b"raw");
        assert_eq!(automatic.backend_connections.load(Ordering::SeqCst), 1);

        let disabled = Tunnel::start(0, false, Some(true)).await?;
        let mut stream = disabled.connect().await?;
        assert_eq!(stream.read(&mut response).await?, 0);
        assert_eq!(disabled.backend_connections.load(Ordering::SeqCst), 0);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn ssh_proxycommand_resumes_without_closing_stdio() -> Result<()> {
    timeout(Duration::from_secs(8), async {
        let tunnel = Tunnel::start(3, false, None).await?;
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_wsproxy"))
            .args(["tunnel", "--server", &tunnel.url])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        input.write_all(b"before").await?;
        let mut response = [0; 6];
        output.read_exact(&mut response).await?;
        assert_eq!(&response, b"before");
        tunnel.online.send(false)?;
        input.write_all(b"after!").await?;
        sleep(Duration::from_millis(100)).await;
        tunnel.online.send(true)?;
        output.read_exact(&mut response).await?;
        assert_eq!(&response, b"after!");
        drop(input);
        let mut rest = Vec::new();
        output.read_to_end(&mut rest).await?;
        assert!(rest.is_empty());
        let result = child.wait_with_output().await?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(tunnel.backend_connections.load(Ordering::SeqCst), 1);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn automatic_client_uses_same_legacy_connection_without_protocol_bytes() -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let mut tasks: JoinSet<Result<()>> = JoinSet::new();
        let (received, checked) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            let (stream, _) = listener.accept().await?;
            // Legacy servers ignore capability headers and forward raw bytes.
            let mut socket = tokio_tungstenite::accept_async(stream).await?;
            socket
                .send(Message::Binary(b"SSH-2.0-legacy\r\n".to_vec().into()))
                .await?;
            let mut data = Vec::new();
            while data.len() < 7 {
                if let Some(Ok(Message::Binary(bytes))) = socket.next().await {
                    data.extend_from_slice(&bytes);
                    socket.send(Message::Binary(bytes)).await?;
                } else {
                    anyhow::bail!("legacy connection ended before application data");
                }
            }
            assert_eq!(data, b"hello\0\xff");
            received.send(()).unwrap();
            assert!(
                timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
            Ok(())
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let client = ProxyClient::bind(listener, format!("ws://{address}"), TlsOptions::default())?;
        tasks.spawn(async move { Ok(client.run().await?) });
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
        let mut greeting = [0; 16];
        stream.read_exact(&mut greeting).await?;
        assert_eq!(&greeting, b"SSH-2.0-legacy\r\n");
        stream.write_all(b"hello\0\xff").await?;
        let mut echoed = [0; 7];
        stream.read_exact(&mut echoed).await?;
        assert_eq!(&echoed, b"hello\0\xff");
        checked.await?;
        tasks.join_next().await.unwrap()??;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn hot_reload_disables_new_sessions_but_existing_sessions_can_resume() -> Result<()> {
    timeout(Duration::from_secs(8), async {
        let tunnel = Tunnel::start(3, false, Some(true)).await?;
        let mut existing = tunnel.connect().await?;
        existing.write_all(b"before").await?;
        let mut response = [0; 6];
        existing.read_exact(&mut response).await?;
        assert_eq!(&response, b"before");

        let path = tunnel.config.as_ref().unwrap().path().join("server.toml");
        let config = wsproxy::ServerFileConfig::load(&path)?;
        std::fs::write(
            path,
            format!(
                "listen = '{}'\ndefault_target = '127.0.0.1:1'\nreconnect_window_secs = 0\n",
                config.listen,
            ),
        )?;
        loop {
            let mut probe = tunnel.connect().await?;
            probe.write_all(b"probe!").await?;
            match probe.read(&mut response).await? {
                0 => break,
                _ => sleep(Duration::from_millis(10)).await,
            }
        }
        let count = tunnel.backend_connections.load(Ordering::SeqCst);
        tunnel.online.send(false)?;
        existing.write_all(b"after!").await?;
        sleep(Duration::from_millis(100)).await;
        tunnel.online.send(true)?;
        existing.read_exact(&mut response).await?;
        assert_eq!(&response, b"after!");
        assert_eq!(tunnel.backend_connections.load(Ordering::SeqCst), count);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
