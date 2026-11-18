//! Versioned, bounded, in-memory stream resumption.
//!
//! Data frames carry an absolute byte offset. ACKs advance only after local
//! writes, so transport loss can discard pending input and replay uncommitted
//! bytes without duplicating output. FIN/FIN-ACK preserve directional EOF.
//! Each attachment exchanges committed offsets before replay begins.
//! Automatic negotiation uses an HTTP capability header so legacy servers can
//! ignore it and forward raw bytes on the same connection. Explicitly required
//! resumption and reattachments use the WebSocket subprotocol without fallback.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

use crate::{Error, Result};

pub(crate) const PROTOCOL: &str = "wsproxy.resume.v1";
pub(crate) const CAPABILITY_HEADER: &str = "x-wsproxy-protocol";
const BUFFER: usize = 1024 * 1024;
const CHUNK: usize = 8192;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT: Duration = Duration::from_secs(5);
const DEAD_PEER: Duration = Duration::from_secs(15);
const MAX_SESSIONS: usize = 1024;
const START: u8 = 0;
const RESUME: u8 = 1;
const READY: u8 = 2;
const REJECT: u8 = 3;
const BUSY: u8 = 4;
const DATA: u8 = 10;
const ACK: u8 = 11;
const FIN: u8 = 12;
const FIN_ACK: u8 = 13;

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub(crate) type Socket = WebSocketStream<Box<dyn Io>>;
type Token = [u8; 32];
pub(crate) type Registry = Arc<Mutex<HashMap<Token, Entry>>>;

pub(crate) struct Entry {
    path: String,
    sender: mpsc::Sender<Attachment>,
    lease: Arc<Semaphore>,
}

struct Attachment {
    socket: Socket,
    acknowledged: u64,
    _permit: tokio::sync::OwnedSemaphorePermit,
    done: oneshot::Sender<()>,
}

pub(crate) async fn accept<S: Io + 'static>(
    stream: S,
    enabled: bool,
) -> Result<(Socket, String, bool)> {
    let mut path = String::new();
    let mut resumable = false;
    #[allow(clippy::result_large_err)]
    let callback = |request: &Request, mut response: Response| {
        path = request.uri().path().to_owned();
        resumable = request
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|p| p.trim() == PROTOCOL));
        if resumable {
            response.headers_mut().insert(
                "sec-websocket-protocol",
                PROTOCOL.parse().expect("static header"),
            );
        } else if enabled
            && request
                .headers()
                .get(CAPABILITY_HEADER)
                .and_then(|v| v.to_str().ok())
                == Some(PROTOCOL)
        {
            resumable = true;
            response
                .headers_mut()
                .insert(CAPABILITY_HEADER, PROTOCOL.parse().expect("static header"));
        }
        Ok(response)
    };
    let stream: Box<dyn Io> = Box::new(stream);
    let socket = tokio_tungstenite::accept_hdr_async(stream, callback).await?;
    Ok((socket, path, resumable))
}

async fn send(socket: &mut Socket, message: Message) -> Result<()> {
    timeout(IO_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| Error::tunnel("WebSocket send timed out"))??;
    Ok(())
}

async fn first_message(socket: &mut Socket) -> Result<Vec<u8>> {
    timeout(IO_TIMEOUT, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Binary(data))) if data.len() <= 128 => return Ok(data.to_vec()),
                Some(Ok(Message::Ping(data))) => send(socket, Message::Pong(data)).await?,
                Some(Ok(Message::Pong(_))) => {}
                Some(Err(e)) => return Err(e.into()),
                _ => return Err(Error::tunnel("invalid or missing resumption handshake")),
            }
        }
    })
    .await
    .map_err(|_| Error::tunnel("resumption handshake timed out"))?
}

fn number(data: &[u8]) -> Result<u64> {
    let bytes = data
        .try_into()
        .map_err(|_| Error::tunnel("invalid protocol integer"))?;
    Ok(u64::from_be_bytes(bytes))
}

fn frame(kind: u8, offset: u64, payload: &[u8]) -> Message {
    let mut data = Vec::with_capacity(9 + payload.len());
    data.push(kind);
    data.extend_from_slice(&offset.to_be_bytes());
    data.extend_from_slice(payload);
    Message::Binary(data.into())
}

enum Outcome {
    Disconnected(Error),
    Finished,
    Failed(Error),
}

struct Endpoint<R, W> {
    reader: R,
    writer: W,
    outgoing: VecDeque<u8>,
    acknowledged: u64,
    received: u64,
    local_eof: bool,
    local_fin_acked: bool,
    remote_eof: bool,
    flush_pending: bool,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Endpoint<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            outgoing: VecDeque::new(),
            acknowledged: 0,
            received: 0,
            local_eof: false,
            local_fin_acked: false,
            remote_eof: false,
            flush_pending: false,
        }
    }

    fn acknowledge(&mut self, offset: u64) -> Result<()> {
        if offset < self.acknowledged || offset > self.sent_end() {
            return Err(Error::tunnel("invalid acknowledgement offset"));
        }
        self.outgoing.drain(..(offset - self.acknowledged) as usize);
        self.acknowledged = offset;
        Ok(())
    }

    fn sent_end(&self) -> u64 {
        self.acknowledged + self.outgoing.len() as u64
    }

    async fn connected(&mut self, socket: &mut Socket) -> Outcome {
        // Only committed local writes are acknowledged. Uncommitted input is
        // discarded on detach and replayed from the peer's retained buffer.
        let mut incoming = VecDeque::new();
        let mut cursor = self.acknowledged;
        let mut ack_dirty = true;
        let mut fin_ack_dirty = self.remote_eof;
        let mut fin_sent = false;
        let mut remote_fin = None;
        let mut last_seen = Instant::now();
        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        let mut buf = [0; CHUNK];
        loop {
            let message = if ack_dirty {
                ack_dirty = false;
                Some(frame(ACK, self.received, &[]))
            } else if fin_ack_dirty {
                fin_ack_dirty = false;
                Some(frame(FIN_ACK, self.received, &[]))
            } else if cursor < self.sent_end() {
                let start = (cursor - self.acknowledged) as usize;
                let end = (start + CHUNK).min(self.outgoing.len());
                let data = self.outgoing.make_contiguous();
                let message = frame(DATA, cursor, &data[start..end]);
                cursor += (end - start) as u64;
                Some(message)
            } else if self.local_eof && !fin_sent {
                fin_sent = true;
                Some(frame(FIN, self.sent_end(), &[]))
            } else {
                None
            };
            if let Some(message) = message {
                if let Err(e) = send(socket, message).await {
                    return Outcome::Disconnected(e);
                }
                continue;
            }
            if self.local_fin_acked && self.remote_eof {
                return Outcome::Finished;
            }
            if remote_fin == Some(self.received)
                && incoming.is_empty()
                && !self.remote_eof
                && !self.flush_pending
            {
                if let Err(e) = self.writer.shutdown().await {
                    return Outcome::Failed(e.into());
                }
                self.remote_eof = true;
                fin_ack_dirty = true;
                continue;
            }
            let available = (BUFFER - self.outgoing.len()).min(CHUNK);
            tokio::select! {
                result = self.reader.read(&mut buf[..available]),
                    if available > 0 && !self.local_eof => {
                    match result {
                        Ok(0) => self.local_eof = true,
                        Ok(n) => self.outgoing.extend(&buf[..n]),
                        Err(e) => return Outcome::Failed(e.into()),
                    }
                }
                result = async {
                    if self.flush_pending {
                        self.writer.flush().await.map(|()| None)
                    } else {
                        self.writer.write(incoming.make_contiguous()).await.map(Some)
                    }
                }, if self.flush_pending || !incoming.is_empty() => {
                    match result {
                        Ok(None) => self.flush_pending = false,
                        Ok(Some(0)) => return Outcome::Failed(Error::tunnel("local stream write returned zero")),
                        Ok(Some(n)) => {
                            incoming.drain(..n);
                            self.received += n as u64;
                            ack_dirty = true;
                            self.flush_pending = true;
                        }
                        Err(e) => return Outcome::Failed(e.into()),
                    }
                }
                message = socket.next() => {
                    last_seen = Instant::now();
                    match message {
                        Some(Ok(Message::Binary(data))) => {
                            if data.len() < 9 {
                                return Outcome::Failed(Error::tunnel("truncated stream frame"));
                            }
                            let offset = match number(&data[1..9]) {
                                Ok(offset) => offset,
                                Err(e) => return Outcome::Failed(e),
                            };
                            match data[0] {
                                DATA => {
                                    if data.len() > CHUNK + 9 || data.len() == 9
                                        || self.remote_eof || remote_fin.is_some()
                                        || offset != self.received + incoming.len() as u64
                                        || incoming.len() + data.len() - 9 > BUFFER {
                                        return Outcome::Failed(Error::tunnel("invalid stream data offset or window"));
                                    }
                                    incoming.extend(&data[9..]);
                                }
                                ACK if data.len() == 9 && offset <= cursor => {
                                    if let Err(e) = self.acknowledge(offset) {
                                        return Outcome::Failed(e);
                                    }
                                }
                                FIN if data.len() == 9
                                    && offset == self.received + incoming.len() as u64 => {
                                    remote_fin = Some(offset);
                                    if self.remote_eof {
                                        fin_ack_dirty = true;
                                    }
                                }
                                FIN_ACK if data.len() == 9 && self.local_eof
                                    && offset == self.sent_end() => {
                                    if let Err(e) = self.acknowledge(offset) {
                                        return Outcome::Failed(e);
                                    }
                                    self.local_fin_acked = true;
                                }
                                _ => return Outcome::Failed(Error::tunnel("invalid stream control frame")),
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            if let Err(e) = send(socket, Message::Pong(data)).await {
                                return Outcome::Disconnected(e);
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Err(e)) => return Outcome::Disconnected(e.into()),
                        Some(Ok(Message::Close(_))) | None =>
                            return Outcome::Disconnected(Error::tunnel("WebSocket disconnected")),
                        _ => return Outcome::Failed(Error::tunnel("unexpected stream message")),
                    }
                }
                _ = heartbeat.tick() => {
                    if last_seen.elapsed() >= DEAD_PEER {
                        return Outcome::Disconnected(Error::tunnel("WebSocket heartbeat timed out"));
                    }
                    if let Err(e) = send(socket, Message::Ping(Vec::new().into())).await {
                        return Outcome::Disconnected(e);
                    }
                }
            }
        }
    }
}

async fn reject(socket: &mut Socket, retryable: bool, reason: &str) -> Result<()> {
    let mut data = vec![if retryable { BUSY } else { REJECT }];
    data.extend_from_slice(reason.as_bytes());
    send(socket, Message::Binary(data.into())).await
}

pub(crate) async fn serve(
    mut socket: Socket,
    path: String,
    target: Option<String>,
    window: Duration,
    registry: Registry,
) -> Result<()> {
    let hello = first_message(&mut socket).await?;
    if hello.first() == Some(&RESUME) && hello.len() == 41 {
        let token: Token = hello[1..33].try_into().expect("validated token length");
        let acknowledged = number(&hello[33..])?;
        let entries = registry.lock().await;
        let Some(entry) = entries.get(&token).filter(|entry| entry.path == path) else {
            drop(entries);
            reject(&mut socket, false, "session expired or unknown").await?;
            return Ok(());
        };
        let sender = entry.sender.clone();
        let permit = entry.lease.clone().try_acquire_owned();
        drop(entries);
        let Ok(permit) = permit else {
            reject(&mut socket, true, "previous connection is still attached").await?;
            return Ok(());
        };
        let (done, finished) = oneshot::channel();
        sender
            .send(Attachment {
                socket,
                acknowledged,
                _permit: permit,
                done,
            })
            .await
            .map_err(|_| Error::tunnel("session expired during attachment"))?;
        finished
            .await
            .map_err(|_| Error::tunnel("session stopped"))?;
        return Ok(());
    }
    if hello != [START] {
        return Err(Error::tunnel("invalid session request"));
    }
    if window.is_zero() {
        reject(&mut socket, false, "resumable tunnels are disabled").await?;
        return Ok(());
    }
    let Some(target) = target else {
        reject(&mut socket, false, "no route for new session").await?;
        return Err(Error::no_route_found(path));
    };
    let tcp = timeout(IO_TIMEOUT, TcpStream::connect(&target))
        .await
        .map_err(|_| Error::tunnel("backend connection timed out"))??;
    let mut token = [0; 32];
    getrandom::getrandom(&mut token)
        .map_err(|e| Error::tunnel(format!("cannot generate session token: {e}")))?;
    let (sender, mut receiver) = mpsc::channel(1);
    let lease = Arc::new(Semaphore::new(1));
    let permit = lease.clone().try_acquire_owned().expect("new semaphore");
    {
        let mut entries = registry.lock().await;
        if entries.len() >= MAX_SESSIONS {
            drop(entries);
            reject(&mut socket, false, "session limit reached").await?;
            return Err(Error::tunnel("resumable session limit reached"));
        }
        entries.insert(
            token,
            Entry {
                path,
                sender,
                lease,
            },
        );
    }
    let (reader, writer) = tcp.into_split();
    let mut endpoint = Endpoint::new(reader, writer);
    let (done, _finished) = oneshot::channel();
    let mut attachment = Attachment {
        socket,
        acknowledged: 0,
        _permit: permit,
        done,
    };
    let result = loop {
        if let Err(e) = endpoint.acknowledge(attachment.acknowledged) {
            if let Err(send_error) =
                reject(&mut attachment.socket, false, "invalid resume offset").await
            {
                eprintln!("Failed to notify tunnel peer: {send_error}");
            }
            let _ = attachment.done.send(());
            break Err(e);
        }
        let mut welcome = vec![READY];
        welcome.extend_from_slice(&token);
        welcome.extend_from_slice(&window.as_secs().to_be_bytes());
        welcome.extend_from_slice(&endpoint.received.to_be_bytes());
        let outcome = match send(&mut attachment.socket, Message::Binary(welcome.into())).await {
            Ok(()) => endpoint.connected(&mut attachment.socket).await,
            Err(e) => Outcome::Disconnected(e),
        };
        match outcome {
            Outcome::Finished => {
                let _ = attachment.done.send(());
                break Ok(());
            }
            Outcome::Failed(e) => {
                if let Err(send_error) =
                    reject(&mut attachment.socket, false, "backend or protocol failure").await
                {
                    eprintln!("Failed to notify tunnel peer: {send_error}");
                }
                let _ = attachment.done.send(());
                break Err(e);
            }
            Outcome::Disconnected(e) => eprintln!("Tunnel disconnected; awaiting resume: {e}"),
        }
        let Attachment {
            done,
            _permit,
            socket,
            ..
        } = attachment;
        drop(socket);
        drop(_permit);
        let _ = done.send(());
        match timeout(window, receiver.recv()).await {
            Ok(Some(next)) => attachment = next,
            _ => break Err(Error::tunnel("reconnect window expired")),
        }
    };
    registry.lock().await.remove(&token);
    result
}

enum Welcome {
    Ready {
        token: Token,
        window: Duration,
        acknowledged: u64,
    },
    Busy,
}

async fn welcome(socket: &mut Socket) -> Result<Welcome> {
    let data = first_message(socket).await?;
    match data.first() {
        Some(&READY) if data.len() == 49 => {
            let seconds = number(&data[33..41])?;
            if seconds == 0 || seconds > u64::from(u32::MAX) {
                return Err(Error::rejected("invalid server reconnect window"));
            }
            Ok(Welcome::Ready {
                token: data[1..33].try_into().expect("validated token length"),
                window: Duration::from_secs(seconds),
                acknowledged: number(&data[41..49])?,
            })
        }
        Some(&BUSY) => Ok(Welcome::Busy),
        Some(&REJECT) => Err(Error::rejected(String::from_utf8_lossy(&data[1..]))),
        _ => Err(Error::tunnel("server did not accept resumable tunnel")),
    }
}

pub(crate) async fn client<R, W>(
    mut socket: Socket,
    server_url: &str,
    tls: &crate::TlsOptions,
    reader: R,
    writer: W,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    send(&mut socket, Message::Binary(vec![START].into())).await?;
    let Welcome::Ready {
        token,
        window,
        acknowledged,
    } = welcome(&mut socket).await?
    else {
        return Err(Error::tunnel("server rejected new session"));
    };
    let mut endpoint = Endpoint::new(reader, writer);
    endpoint.acknowledge(acknowledged)?;
    loop {
        match endpoint.connected(&mut socket).await {
            Outcome::Finished => return Ok(()),
            Outcome::Failed(e) => return Err(e),
            Outcome::Disconnected(e) => eprintln!("Tunnel disconnected; reconnecting: {e}"),
        }
        let deadline = Instant::now() + window;
        let mut delay = Duration::from_millis(100);
        loop {
            let attempt = async {
                let mut next =
                    crate::client::connect(server_url, tls, crate::client::Resumption::Required)
                        .await?
                        .socket;
                let mut hello = vec![RESUME];
                hello.extend_from_slice(&token);
                hello.extend_from_slice(&endpoint.received.to_be_bytes());
                send(&mut next, Message::Binary(hello.into())).await?;
                let response = welcome(&mut next).await?;
                Ok::<_, Error>((next, response))
            };
            match timeout_at(deadline, attempt).await {
                Ok(Ok((
                    next,
                    Welcome::Ready {
                        token: returned,
                        acknowledged,
                        ..
                    },
                ))) => {
                    if returned != token {
                        return Err(Error::tunnel("server replaced the session"));
                    }
                    endpoint.acknowledge(acknowledged)?;
                    socket = next;
                    eprintln!("Tunnel reconnected");
                    break;
                }
                Ok(Ok((_, Welcome::Busy))) => {}
                Ok(Err(e)) => {
                    // A server rejection is terminal; transport failures are retryable.
                    if e.is_tunnel_rejection() {
                        return Err(e);
                    }
                    eprintln!("Reconnect attempt failed: {e}");
                }
                Err(_) => return Err(Error::tunnel("reconnect window expired")),
            }
            let mut jitter = [0; 1];
            getrandom::getrandom(&mut jitter)
                .map_err(|e| Error::tunnel(format!("cannot generate retry jitter: {e}")))?;
            let wait = delay + Duration::from_millis(u64::from(jitter[0]));
            if timeout_at(deadline, tokio::time::sleep(wait))
                .await
                .is_err()
            {
                return Err(Error::tunnel("reconnect window expired"));
            }
            delay = (delay * 2).min(Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::task::JoinSet;
    use tokio_tungstenite::tungstenite::protocol::Role;

    async fn pair() -> (Socket, Socket) {
        let (client, server) = tokio::io::duplex(CHUNK * 4);
        let client: Box<dyn Io> = Box::new(client);
        let server: Box<dyn Io> = Box::new(server);
        (
            WebSocketStream::from_raw_socket(client, Role::Client, None).await,
            WebSocketStream::from_raw_socket(server, Role::Server, None).await,
        )
    }

    async fn attach(
        tasks: &mut JoinSet<Result<()>>,
        registry: &Registry,
        target: &str,
        path: &str,
    ) -> Socket {
        let (client, server) = pair().await;
        let registry = registry.clone();
        let target = Some(target.to_owned());
        let path = path.to_owned();
        tasks.spawn(serve(
            server,
            path,
            target,
            Duration::from_secs(1),
            registry,
        ));
        client
    }

    async fn resume_request(socket: &mut Socket, token: Token, received: u64) -> Result<()> {
        let mut hello = vec![RESUME];
        hello.extend_from_slice(&token);
        hello.extend_from_slice(&received.to_be_bytes());
        send(socket, Message::Binary(hello.into())).await
    }

    async fn next_data(socket: &mut Socket) -> Result<Vec<u8>> {
        loop {
            let data = first_message(socket).await?;
            if data[0] == DATA {
                return Ok(data);
            }
        }
    }

    async fn detached(registry: &Registry, token: Token) {
        loop {
            if registry
                .lock()
                .await
                .get(&token)
                .unwrap()
                .lease
                .available_permits()
                == 1
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn replay_offsets_attachment_ownership_and_expiry() -> Result<()> {
        timeout(Duration::from_secs(5), async {
            let mut tasks = JoinSet::new();
            let registry = Registry::default();
            let backend = TcpListener::bind("127.0.0.1:0").await?;
            let target = backend.local_addr()?.to_string();
            let (committed, written) = oneshot::channel();
            let backend_task = tokio::spawn(async move {
                let (mut tcp, _) = backend.accept().await?;
                tcp.write_all(b"greeting").await?;
                let mut input = [0; 6];
                tcp.read_exact(&mut input).await?;
                assert_eq!(&input, b"abcdef");
                committed.send(()).unwrap();
                let mut rest = Vec::new();
                tcp.read_to_end(&mut rest).await?;
                assert!(rest.is_empty(), "committed bytes were duplicated");
                assert!(
                    timeout(Duration::from_millis(30), backend.accept())
                        .await
                        .is_err()
                );
                Ok::<_, Error>(())
            });

            let mut first = attach(&mut tasks, &registry, &target, "/ssh").await;
            send(&mut first, Message::Binary(vec![START].into())).await?;
            let Welcome::Ready { token, .. } = welcome(&mut first).await? else {
                panic!("new session rejected");
            };
            let data = next_data(&mut first).await?;
            assert_eq!(number(&data[1..9])?, 0);
            assert_eq!(&data[9..], b"greeting");

            let mut busy = attach(&mut tasks, &registry, &target, "/ssh").await;
            resume_request(&mut busy, token, 0).await?;
            assert!(matches!(welcome(&mut busy).await?, Welcome::Busy));
            let mut wrong_path = attach(&mut tasks, &registry, &target, "/other").await;
            resume_request(&mut wrong_path, token, 0).await?;
            assert!(matches!(welcome(&mut wrong_path).await, Err(e) if e.is_tunnel_rejection()));

            // Neither the greeting nor this input is acknowledged before loss.
            send(&mut first, frame(DATA, 0, b"abcdef")).await?;
            written.await.unwrap();
            drop(first);
            detached(&registry, token).await;

            let mut second = attach(&mut tasks, &registry, &target, "/ssh").await;
            resume_request(&mut second, token, 3).await?;
            let Welcome::Ready { acknowledged, .. } = welcome(&mut second).await? else {
                panic!("resume rejected");
            };
            assert_eq!(acknowledged, 6);
            let replay = next_data(&mut second).await?;
            assert_eq!(number(&replay[1..9])?, 3);
            assert_eq!(&replay[9..], b"eting");
            drop(second);

            backend_task
                .await
                .map_err(|e| Error::tunnel(e.to_string()))??;
            assert!(registry.lock().await.is_empty());
            let mut expired = attach(&mut tasks, &registry, &target, "/ssh").await;
            resume_request(&mut expired, token, 8).await?;
            assert!(matches!(welcome(&mut expired).await, Err(e) if e.is_tunnel_rejection()));
            Ok(())
        })
        .await
        .map_err(|_| Error::tunnel("protocol test timed out"))?
    }

    #[tokio::test]
    async fn unacknowledged_data_is_bounded_and_backpressures_local_input() -> Result<()> {
        timeout(Duration::from_secs(5), async {
            let (mut socket, mut peer) = pair().await;
            let (local, mut application) = tokio::io::duplex(CHUNK);
            let (reader, writer) = tokio::io::split(local);
            let task = tokio::spawn(async move {
                let mut endpoint = Endpoint::new(reader, writer);
                let outcome = endpoint.connected(&mut socket).await;
                (endpoint, outcome)
            });
            let write =
                tokio::spawn(async move { application.write_all(&vec![7; BUFFER * 2]).await });
            let mut received = 0;
            while received < BUFFER {
                match peer.next().await {
                    Some(Ok(Message::Binary(data))) if data[0] == DATA => {
                        received += data.len() - 9
                    }
                    Some(Ok(Message::Ping(_))) => peer.flush().await?,
                    Some(Ok(_)) => {}
                    other => panic!("unexpected message: {other:?}"),
                }
            }
            assert!(!write.is_finished());
            drop(peer);
            let (mut endpoint, outcome) = task.await.map_err(|e| Error::tunnel(e.to_string()))?;
            assert!(matches!(outcome, Outcome::Disconnected(_)));
            assert_eq!(endpoint.outgoing.len(), BUFFER);
            assert!(endpoint.acknowledge(BUFFER as u64 + 1).is_err());
            endpoint.acknowledge(3)?;
            assert!(endpoint.acknowledge(2).is_err());
            write.abort();
            Ok(())
        })
        .await
        .map_err(|_| Error::tunnel("backpressure test timed out"))?
    }
}
