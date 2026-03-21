# wsproxy

A WebSocket proxy for TCP connections. Forward TCP traffic through WebSocket tunnels.

## Architecture

```
TCP Client <---> ProxyClient <--WebSocket--> ProxyServer <---> TCP Server
```

- **ProxyServer**: Listens for WebSocket connections and forwards data to TCP targets. Supports routing different URL paths to different backends.
- **ProxyClient**: Listens for TCP connections and forwards data through WebSocket to the server.

## Installation

```bash
cargo install --path .
```

## Usage

### Command Line

**Server with a single default target:**

```bash
wsproxy server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
```

**Server with multiple routes:**

```bash
wsproxy server --listen 0.0.0.0:8080 \
  --route /ssh=127.0.0.1:22 \
  --route /db=127.0.0.1:5432 \
  --route /redis=127.0.0.1:6379
```

**Client:**

```bash
wsproxy client --listen 127.0.0.1:2222 --server ws://proxy-server:8080/ssh
```

### Daemon Mode

Run the server or client as a background daemon with automatic restart on failure:

**Start a server daemon:**

```bash
wsproxy daemon server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
```

**Start a client daemon:**

```bash
wsproxy daemon client --listen 127.0.0.1:2222 --server ws://proxy-server:8080/ssh
```

**List running daemons:**

```bash
wsproxy daemon list
```

Output:
```
ID   PID      ARGUMENTS
--------------------------------------------------
1    12345    server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
2    12346    client --listen 127.0.0.1:2222 --server ws://proxy-server:8080/ssh
```

**Kill a daemon:**

```bash
wsproxy daemon kill 1
```

Daemons automatically restart with exponential backoff (1ms to 5 minutes) if the underlying process crashes.

### Library

```rust
use wsproxy::{ProxyServer, ProxyClient};

#[tokio::main]
async fn main() -> wsproxy::Result<()> {
    // Server with multiple routes
    let server = ProxyServer::builder()
        .route("/ssh", "127.0.0.1:22")?
        .route("/db", "127.0.0.1:5432")?
        .bind("0.0.0.0:8080")?;

    // Client
    let client = ProxyClient::bind(
        "127.0.0.1:2222",
        "ws://proxy-server:8080/ssh",
    )?;

    // Run (typically in separate processes)
    // server.run().await?;
    // client.run().await?;
    Ok(())
}
```

### Example: Simple Chat with netcat

This example demonstrates a simple chat through the WebSocket proxy using `nc` (netcat).

**Terminal 1 - Start a TCP listener (the "chat server"):**

```bash
nc -l 9000
```

**Terminal 2 - Start the proxy server and client as daemons:**

```bash
wsproxy daemon server --listen 127.0.0.1:8080 --default-target 127.0.0.1:9000
wsproxy daemon client --listen 127.0.0.1:2222 --server ws://127.0.0.1:8080
```

**Terminal 2 - Connect to the proxy:**

```bash
nc 127.0.0.1 2222
```

Now you can type messages in Terminal 2 and they will appear in Terminal 1 (and vice versa). The data flows:

```
Terminal 2 (nc) -> ProxyClient (:2222) -> WebSocket -> ProxyServer (:8080) -> Terminal 1 (nc :9000)
```

**Clean up - kill the daemons:**

```bash
wsproxy daemon list   # See running daemons
wsproxy daemon kill 1 # Kill server
wsproxy daemon kill 2 # Kill client
```

## License

MIT License - see [LICENSE](LICENSE) for details.
