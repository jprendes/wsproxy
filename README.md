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

### SSH ProxyCommand (Tunnel Mode)

The `tunnel` command connects stdin/stdout directly to a WebSocket server, making it perfect for SSH's `ProxyCommand` option:

**Direct tunnel:**

```bash
wsproxy tunnel --server ws://proxy-server:8080/ssh
```

**SSH config example:**

```
Host myserver
    ProxyCommand wsproxy tunnel --server wss://proxy:8443/ssh
    User myuser
```

**With self-signed certificate:**

```
Host myserver
    ProxyCommand wsproxy tunnel --server wss://proxy:8443/ssh --insecure
    User myuser
```

Now `ssh myserver` will tunnel through the WebSocket proxy.

### Automatic Reconnection

Resumable tunnels preserve the **existing SSH connection** across temporary
network failures. Updated clients automatically negotiate resumption when the
server enables it, and use raw forwarding with older or disabled servers.

Enable it in the server's TOML file (at the top level, before any tables):

```toml
listen = "0.0.0.0:8443"
default_target = "127.0.0.1:22"
reconnect_window = 120

[tls]
cert = "cert.pem"
key = "key.pem"
```

Your existing SSH configuration works without additional client flags:

```text
Host myserver
    ProxyCommand wsproxy tunnel --server wss://proxy:8443/ssh
    User myuser
```

Or in TCP listener mode:

```bash
wsproxy client --listen 127.0.0.1:2222 --server wss://proxy:8443/ssh
```

The client automatically uses the server's reconnect window; no matching client
timeout is needed. During an outage, SSH pauses rather than disconnecting.
Reconnection uses exponential backoff with jitter. Heartbeats run every 5 seconds
and detect an unresponsive peer after approximately 15 seconds; individual
connection/handshake/send operations time out after 10 seconds. The reconnect
window starts when a disconnect is detected.

- `reconnect_window` is an integer number of **seconds**, defaulting to `0`
  (disabled). The old name `reconnect_window_secs` remains accepted as an alias;
  do not specify both names. It can be hot-reloaded for
  **new sessions**; existing sessions retain their original window, including if
  resumption is subsequently disabled.
- Old clients continue to work unchanged with updated servers, without a
  reconnect window. Updated clients negotiate via HTTP upgrade headers, before
  sending application data; an older server receives no protocol probe bytes
  and needs no extra connection.
- `--resume` is optional: it **requires** resumption and fails explicitly against
  a server that does not support or enable it. Automatic raw fallback is only
  allowed when first connecting; an established resumable session never falls
  back to a replacement TCP connection.
- Each direction retains at most 1 MiB of unacknowledged TCP data and applies
  backpressure when full. Offsets and acknowledgements prevent lost or duplicate
  bytes after reconnection. TCP half-closes are preserved.
- The server limits resumable sessions to 1024, including disconnected sessions.
  Expired sessions are closed and removed.
- Use **WSS with certificate verification**: resume tokens grant access to the
  existing tunnel. Do not expose a plain WS connection over an untrusted network.
- Resumption requires both proxy processes and the original backend TCP
  connection to stay alive. With multiple server instances, reconnects must reach
  the same instance. Process/container restarts and backend/SSH keepalive timeouts
  cannot be recovered; configure SSH timeouts to tolerate your intended outages.
- In the library, enable the server with
  `ProxyServer::builder().reconnect_window_secs(120)`. `ProxyClient::bind(...)`
  and `client::tunnel(...)` negotiate automatically. Use
  `.with_resumption(true)` or `client::tunnel_resumable(...)` to require
  resumption, or `.with_resumption(false)` to force raw TCP forwarding.

#### Per-route reconnect windows

Use optional overrides while keeping a global default:

```toml
listen = "0.0.0.0:8022"
reconnect_window = 600

[routes]
"/ssh/{host}" = "{host}:22"
"/gringotts" = "localhost:22"
"/db" = "localhost:5432"

[reconnect_windows]
"/ssh/{host}" = 1200
"/db" = 0
```

The override key must exactly match a configured route **pattern**, not an
individual requested URL. Unknown keys and invalid values are rejected.
Here, `/ssh/cube` gets 20 minutes, `/gringotts` inherits 10 minutes, and `/db`
uses raw forwarding without resumption. A more-specific route without an override
inherits the global value, not an override on a less-specific pattern.
Unmatched paths served by `default_target` also use the global value.
Overrides can enable resumption even when the global value is `0`.
Trailing-slash normalization follows the same rules as routing.

Negotiation selects the route's window before any application data is sent.
Hot reloads affect new sessions only; existing sessions retain their original
backend and window, including when a route is changed, removed, or disabled.
The Rust builder's `reconnect_window_secs(...)` method retains its explicit unit
name; per-route overrides are configured through TOML.

### Docker

Run wsproxy in a container with automatic restart and startup on boot. See [docs/docker.md](docs/docker.md).

### Upstart

For systems using Upstart (Ubuntu 14.04 and earlier), see [docs/upstart.md](docs/upstart.md).

### systemd

For systems using systemd (most modern Linux distributions), see [docs/systemd.md](docs/systemd.md).

### Configuration File

Instead of command-line arguments, you can use a TOML configuration file with **hot-reload support**. When the config file changes, the server automatically picks up the new configuration without dropping active connections.

**Start server with config file:**

```bash
wsproxy server --config server.toml
```

**Example configuration file (`server.toml`):**

```toml
listen = "0.0.0.0:8080"
default_target = "localhost:22"

[routes]
"/ssh" = "ssh-server.internal:22"
"/db" = "db.example.com:5432"
"/redis" = "127.0.0.1:6379"

[tls]
cert = "cert.pem"
key = "key.pem"
# Or use: self_signed = true
```

Hostnames are resolved using DNS when each connection is established, allowing DNS changes to take effect without restarting the server.

**Hot-reload behavior:**

- **Routes and default_target changes**: Applied instantly. Existing connections continue uninterrupted; new connections use the updated routing.
- **Reconnect window changes**: Applied to new resumable sessions. Existing sessions keep their original window and backend, even if routes change.
- **Listen address or TLS changes**: Server automatically restarts. Existing connections continue until they complete naturally.
- **Invalid configuration**: If the config file has syntax errors or invalid values, the error is logged and the server continues running with the previous valid configuration. Existing connections are not affected.

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
        .default_target("127.0.0.1:22")
        .tls("cert.pem", "key.pem")
        .bind("0.0.0.0:8443")?;

    // Server with auto-generated self-signed certificate
    let dev_server = ProxyServer::builder()
        .default_target("127.0.0.1:22")
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

**Terminal 2 - Start the proxy server:**

```bash
wsproxy server --listen 127.0.0.1:8080 --default-target 127.0.0.1:9000
```

**Terminal 3 - Start the proxy client:**

```bash
wsproxy client --listen 127.0.0.1:2222 --server ws://127.0.0.1:8080
```

**Terminal 4 - Connect to the proxy:**

```bash
nc 127.0.0.1 2222
```

Now you can type messages in Terminal 4 and they will appear in Terminal 1 (and vice versa). The data flows:

```
Terminal 4 (nc) -> ProxyClient (:2222) -> WebSocket -> ProxyServer (:8080) -> Terminal 1 (nc :9000)
```

Press Ctrl+C in terminals 2 and 3 to stop the proxies (graceful shutdown).

## License

MIT License - see [LICENSE](LICENSE) for details.
