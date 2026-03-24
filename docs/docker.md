# Docker Configuration

Run wsproxy as a containerized service with automatic restart and startup on boot.

Note: Examples use `--network host` so that localhost works as expected. This works with Docker Engine on Linux. On macOS, Windows, or Docker Desktop (including on Linux), use the default bridge networking with `-p` port mappings and `host.docker.internal` instead of `127.0.0.1`.

## Pre-built Image

Pull the pre-built image from GitHub Container Registry:

```bash
docker pull ghcr.io/jprendes/wsproxy:latest
```

## Building Locally

Alternatively, build the Docker image from the project root:

```bash
docker build -t ghcr.io/jprendes/wsproxy .
```

## Basic Server

```bash
docker run -d --name wsproxy-server \
  --restart unless-stopped \
  --network host \
  ghcr.io/jprendes/wsproxy:latest \
  server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
```

The `--restart unless-stopped` policy ensures the container automatically restarts on failure and starts on system boot.

## Server with TLS

```bash
docker run -d --name wsproxy-server \
  --restart unless-stopped \
  --network host \
  -v /path/to/certs:/certs:ro \
  ghcr.io/jprendes/wsproxy:latest \
  server --listen 0.0.0.0:8443 --default-target 127.0.0.1:22 \
  --tls-cert /certs/cert.pem --tls-key /certs/key.pem
```

## Server with Self-Signed Certificate

```bash
docker run -d --name wsproxy-server \
  --restart unless-stopped \
  --network host \
  ghcr.io/jprendes/wsproxy:latest \
  server --listen 0.0.0.0:8443 --default-target 127.0.0.1:22 \
  --tls-self-signed
```

## Client

```bash
docker run -d --name wsproxy-client \
  --restart unless-stopped \
  --network host \
  ghcr.io/jprendes/wsproxy:latest \
  client --listen 127.0.0.1:2222 --server ws://localhost:8080/ssh
```

## Using a Config File

```bash
docker run -d --name wsproxy-server \
  --restart unless-stopped \
  --network host \
  -v /path/to/server.toml:/config/server.toml:ro \
  ghcr.io/jprendes/wsproxy:latest \
  server --config /config/server.toml
```

See [Configuration File](../README.md#configuration-file) for config file format.

## Managing the Container

```bash
docker start wsproxy-server    # Start the container
docker stop wsproxy-server     # Stop the container (graceful shutdown)
docker restart wsproxy-server  # Restart the container
docker logs -f wsproxy-server  # Follow logs
docker rm wsproxy-server       # Remove the container
```
