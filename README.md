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

**Server with TLS (WSS):**

```bash
wsproxy server --listen 0.0.0.0:8443 \
  --default-target 127.0.0.1:22 \
  --tls-cert cert.pem \
  --tls-key key.pem
```

**Server with auto-generated self-signed certificate:**

```bash
wsproxy server --listen 0.0.0.0:8443 \
  --default-target 127.0.0.1:22 \
  --tls-self-signed
```

**Client:**

```bash
wsproxy client --listen 127.0.0.1:2222 --server ws://proxy-server:8080/ssh
```

**Client connecting to WSS server:**

```bash
wsproxy client --listen 127.0.0.1:2222 --server wss://proxy-server:8443/ssh
```

**Client with self-signed certificate (insecure mode):**

```bash
wsproxy client --listen 127.0.0.1:2222 --server wss://proxy-server:8443/ssh --insecure
```

**Client with custom CA certificate:**

```bash
wsproxy client --listen 127.0.0.1:2222 --server wss://proxy-server:8443/ssh \
  --tls-ca-cert /path/to/ca.pem
```

### Daemon Mode

Run the server or client as a background daemon with automatic restart on failure:

**Start a server daemon:**

```bash
wsproxy daemon server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
```

**Start a server daemon with TLS:**

```bash
wsproxy daemon server --listen 0.0.0.0:8443 --default-target 127.0.0.1:22 \
  --tls-cert cert.pem --tls-key key.pem
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

### Configuration File

Instead of command-line arguments, you can use a TOML configuration file with **hot-reload support**. When the config file changes, the server automatically picks up the new configuration without dropping active connections.

**Start server with config file:**

```bash
wsproxy server --config server.toml
```

**Example configuration file (`server.toml`):**

```toml
listen = "0.0.0.0:8080"
default_target = "127.0.0.1:22"

[routes]
"/ssh" = "127.0.0.1:22"
"/db" = "127.0.0.1:5432"
"/redis" = "127.0.0.1:6379"

[tls]
cert = "cert.pem"
key = "key.pem"
# Or use: self_signed = true
```

**Hot-reload behavior:**

- **Routes and default_target changes**: Applied instantly. Existing connections continue uninterrupted; new connections use the updated routing.
- **Listen address or TLS changes**: Server automatically restarts. Existing connections continue until they complete naturally.
- **Invalid configuration**: If the config file has syntax errors or invalid values, the error is logged and the server continues running with the previous valid configuration. Existing connections are not affected.

**Daemon mode with config:**

```bash
wsproxy daemon server --config server.toml
```

Note: The `--config` flag cannot be combined with other server options (`--listen`, `--route`, `--default-target`, `--tls-*`).

### Library

```rust
use wsproxy::{ProxyServer, ProxyClient, TlsOptions};

#[tokio::main]
async fn main() -> wsproxy::Result<()> {
    // Server with multiple routes
    let server = ProxyServer::builder()
        .route("/ssh", "127.0.0.1:22")?
        .route("/db", "127.0.0.1:5432")?
        .bind("0.0.0.0:8080")?;

    // Server with TLS (WSS)
    let secure_server = ProxyServer::builder()
        .default_target("127.0.0.1:22")?
        .tls("cert.pem", "key.pem")
        .bind("0.0.0.0:8443")?;

    // Server with auto-generated self-signed certificate
    let dev_server = ProxyServer::builder()
        .default_target("127.0.0.1:22")?
        .tls_self_signed()
        .bind("0.0.0.0:8443")?;

    // Client (supports both ws:// and wss://)
    let client = ProxyClient::bind(
        "127.0.0.1:2222",
        "wss://proxy-server:8443/ssh",
        TlsOptions::default(),
    )?;

    // Client with custom CA certificate (for self-signed servers)
    let client_custom_ca = ProxyClient::bind(
        "127.0.0.1:2222",
        "wss://proxy-server:8443/ssh",
        TlsOptions {
            insecure: false,
            ca_cert_path: Some("/path/to/ca.pem".to_string()),
        },
    )?;

    // Run server with config file (supports hot-reload)
    // wsproxy::server::run_with_config("server.toml").await?;

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
