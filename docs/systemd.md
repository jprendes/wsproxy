# systemd Configuration

For systems using systemd (most modern Linux distributions), you can configure wsproxy to run as a system service.

## Basic Setup

Create `/etc/systemd/system/wsproxy.service`:

```ini
[Unit]
Description=wsproxy WebSocket proxy
After=network.target
; Stop restarting if it restarts more than 10 times in 5 seconds
StartLimitBurst=10
StartLimitIntervalSec=5

[Service]
Type=simple
ExecStart=/usr/local/bin/wsproxy server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

## Enable and Start

```bash
sudo systemctl daemon-reload
sudo systemctl enable wsproxy
sudo systemctl start wsproxy
```

## Managing the Service

```bash
sudo systemctl start wsproxy    # Start the service
sudo systemctl stop wsproxy     # Stop the service
sudo systemctl restart wsproxy  # Restart the service
sudo systemctl status wsproxy   # Check status
journalctl -u wsproxy -f        # Follow logs
```

## Using a Config File

For more complex configurations, use a TOML config file:

```ini
[Unit]
Description=wsproxy WebSocket proxy
After=network.target
; Stop restarting if it restarts more than 10 times in 5 seconds
StartLimitBurst=10
StartLimitIntervalSec=5

[Service]
Type=simple
ExecStart=/usr/local/bin/wsproxy server --config /etc/wsproxy/server.toml
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

See [Configuration File](../README.md#configuration-file) for config file format.

## Multiple Instances

To run multiple wsproxy instances, use template units.

Create `/etc/systemd/system/wsproxy@.service`:

```ini
[Unit]
Description=wsproxy WebSocket proxy (%i)
After=network.target
; Stop restarting if it restarts more than 10 times in 5 seconds
StartLimitBurst=10
StartLimitIntervalSec=5

[Service]
Type=simple
ExecStart=/usr/local/bin/wsproxy server --config /etc/wsproxy/%i.toml
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Then enable instances:

```bash
sudo systemctl enable wsproxy@ssh
sudo systemctl enable wsproxy@db
sudo systemctl start wsproxy@ssh
sudo systemctl start wsproxy@db
```

Each instance uses its own config file (`/etc/wsproxy/ssh.toml`, `/etc/wsproxy/db.toml`).

## Running as Non-Root

For security, run wsproxy as a dedicated user:

```ini
[Unit]
Description=wsproxy WebSocket proxy
After=network.target
; Stop restarting if it restarts more than 10 times in 5 seconds
StartLimitBurst=10
StartLimitIntervalSec=5

[Service]
Type=simple
User=wsproxy
Group=wsproxy
ExecStart=/usr/local/bin/wsproxy server --config /etc/wsproxy/server.toml
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Create the user:

```bash
sudo useradd -r -s /usr/sbin/nologin wsproxy
```

Note: Binding to ports below 1024 requires root or `CAP_NET_BIND_SERVICE` capability.
