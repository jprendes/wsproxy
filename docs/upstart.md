# Upstart Configuration

For systems using Upstart (Ubuntu 14.04 and earlier), you can configure wsproxy to run as a system service.

## Basic Setup

Create `/etc/init/wsproxy.conf`:

```conf
description "wsproxy WebSocket proxy"

# Start on normal runlevels (2=multi-user, 3=networking, 4=custom, 5=GUI)
start on runlevel [2345]
stop on runlevel [!2345]

respawn
# Stop respawning if it restarts more than 10 times in 5 seconds
respawn limit 10 5

exec /usr/local/bin/wsproxy server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
```

## Managing the Service

```bash
sudo start wsproxy    # Start the service
sudo stop wsproxy     # Stop the service
sudo restart wsproxy  # Restart the service
sudo status wsproxy   # Check status
```

## Using a Config File

For more complex configurations, use a TOML config file:

```conf
description "wsproxy WebSocket proxy"

# Start on normal runlevels (2=multi-user, 3=networking, 4=custom, 5=GUI)
start on runlevel [2345]
stop on runlevel [!2345]

respawn
# Stop respawning if it restarts more than 10 times in 5 seconds
respawn limit 10 5

exec /usr/local/bin/wsproxy server --config /etc/wsproxy/server.toml
```

See [Configuration File](../README.md#configuration-file) for config file format.

## Multiple Instances

To run multiple wsproxy instances, create separate conf files:

**`/etc/init/wsproxy-server.conf`:**

```conf
description "wsproxy server"

# Start on normal runlevels (2=multi-user, 3=networking, 4=custom, 5=GUI)
start on runlevel [2345]
stop on runlevel [!2345]

respawn
# Stop respawning if it restarts more than 10 times in 5 seconds
respawn limit 10 5

exec /usr/local/bin/wsproxy server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
```

**`/etc/init/wsproxy-client.conf`:**

```conf
description "wsproxy client"

# Start on normal runlevels (2=multi-user, 3=networking, 4=custom, 5=GUI)
start on runlevel [2345]
stop on runlevel [!2345]

respawn
# Stop respawning if it restarts more than 10 times in 5 seconds
respawn limit 10 5

exec /usr/local/bin/wsproxy client --listen 127.0.0.1:2222 --server ws://proxy-server:8080/ssh
```

## Logging

Upstart logs are typically written to `/var/log/upstart/wsproxy.log`. To customize logging:

```conf
description "wsproxy WebSocket proxy"

# Start on normal runlevels (2=multi-user, 3=networking, 4=custom, 5=GUI)
start on runlevel [2345]
stop on runlevel [!2345]

respawn
# Stop respawning if it restarts more than 10 times in 5 seconds
respawn limit 10 5

console log

exec /usr/local/bin/wsproxy server --listen 0.0.0.0:8080 --default-target 127.0.0.1:22
```
