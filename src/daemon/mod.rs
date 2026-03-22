//! Daemon mode functionality for automatic restart with exponential backoff.
//!
//! Supports running both server and client as daemons with:
//! - Automatic restart on failure with exponential backoff
//! - Cross-platform daemon registry for listing and killing daemons

mod registry;

use std::process::{Command, Stdio};
use std::time::Duration;

pub use registry::{DaemonInfo, DaemonRole};

/// Environment variable used to indicate we're running as the daemon subprocess
const DAEMON_ENV_VAR: &str = "__WSPROXY_DAEMON_CHILD";

/// Environment variable to indicate the process should monitor stdin for parent death
const MONITOR_STDIN_VAR: &str = "__WSPROXY_MONITOR_STDIN";

/// Environment variable containing the daemon ID
const DAEMON_ID_VAR: &str = "__WSPROXY_DAEMON_ID";

/// Check if this process is running as the daemon child (restart loop)
pub fn is_daemon_child() -> bool {
    std::env::var(DAEMON_ENV_VAR).is_ok()
}

/// Check if the process should monitor stdin for parent death
pub fn should_monitor_stdin() -> bool {
    std::env::var(MONITOR_STDIN_VAR).is_ok()
}

/// Get the daemon ID from environment (for cleanup on exit)
fn get_daemon_id() -> Option<u32> {
    std::env::var(DAEMON_ID_VAR).ok()?.parse().ok()
}

/// Run the daemon restart loop with exponential backoff.
/// This function never returns - it continuously restarts the subprocess.
pub fn run_restart_loop() -> ! {
    const MIN_BACKOFF_MS: u64 = 1;
    const MAX_BACKOFF_MS: u64 = 5 * 60 * 1000; // 5 minutes

    let args: Vec<String> = std::env::args().collect();

    // Find "daemon" in args and get the subcommand (server/client) and its args
    // Original: wsproxy daemon server --listen ...
    // We need to run: wsproxy server --listen ...
    let mut child_args: Vec<String> = Vec::new();
    let mut found_daemon = false;
    for arg in &args {
        if found_daemon {
            child_args.push(arg.clone());
        } else if arg == "daemon" {
            found_daemon = true;
            // Skip "daemon", next args will be "server ..." or "client ..."
        } else {
            child_args.push(arg.clone());
        }
    }

    // Determine role from first arg after daemon
    let role = if child_args.get(1).map(|s| s.as_str()) == Some("client") {
        "client"
    } else {
        "server"
    };

    // Set up cleanup on exit
    let daemon_id = get_daemon_id();
    ctrlc::set_handler(move || {
        if let Some(id) = daemon_id {
            registry::unregister(id).ok();
        }
        std::process::exit(0);
    })
    .ok();

    let mut backoff_ms = MIN_BACKOFF_MS;

    loop {
        eprintln!("Starting wsproxy {}...", role);

        // Use piped stdin - when this process dies, stdin closes,
        // and the child will detect EOF
        let mut child = match Command::new(&child_args[0])
            .args(&child_args[1..])
            .env_remove(DAEMON_ENV_VAR)
            .env_remove(DAEMON_ID_VAR)
            .env(MONITOR_STDIN_VAR, "1")
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                eprintln!("Failed to start wsproxy {}: {}", role, e);
                eprintln!("Restarting in {} ms...", backoff_ms);
                std::thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                continue;
            }
        };

        // Take stdin handle - holding it keeps the pipe open
        // When this daemon dies, the handle is dropped and stdin closes
        let _stdin_handle = child.stdin.take();

        let status = child.wait();

        match status {
            Ok(status) if status.success() => {
                // Clean exit, reset backoff
                backoff_ms = MIN_BACKOFF_MS;
                eprintln!("wsproxy {} exited successfully", role);
            }
            Ok(status) => {
                eprintln!("wsproxy {} exited with status: {}", role, status);
            }
            Err(e) => {
                eprintln!("Failed to wait for wsproxy {}: {}", role, e);
            }
        }

        eprintln!("Restarting in {} ms...", backoff_ms);
        std::thread::sleep(Duration::from_millis(backoff_ms));

        // Exponential backoff with cap
        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
    }
}

/// Spawn a detached daemon process for server
pub fn spawn_server(
    config: Option<String>,
    listen: Option<String>,
    route: Vec<String>,
    default_target: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    tls_self_signed: bool,
) -> wsproxy::Result<()> {
    let mut args = vec!["server".to_string()];

    if let Some(config_path) = &config {
        args.push("--config".to_string());
        args.push(config_path.clone());
    } else {
        if let Some(listen_addr) = &listen {
            args.push("--listen".to_string());
            args.push(listen_addr.clone());
        }

        for r in &route {
            args.push("--route".to_string());
            args.push(r.clone());
        }

        if let Some(target) = &default_target {
            args.push("--default-target".to_string());
            args.push(target.clone());
        }

        if let Some(cert) = &tls_cert {
            args.push("--tls-cert".to_string());
            args.push(cert.clone());
        }

        if let Some(key) = &tls_key {
            args.push("--tls-key".to_string());
            args.push(key.clone());
        }

        if tls_self_signed {
            args.push("--tls-self-signed".to_string());
        }
    }

    spawn_daemon(DaemonRole::Server, args)
}

/// Spawn a detached daemon process for client
pub fn spawn_client(
    listen: String,
    server: String,
    insecure: bool,
    tls_ca_cert: Option<String>,
) -> wsproxy::Result<()> {
    let mut args = vec![
        "client".to_string(),
        "--listen".to_string(),
        listen,
        "--server".to_string(),
        server,
    ];

    if insecure {
        args.push("--insecure".to_string());
    }

    if let Some(ca_cert) = &tls_ca_cert {
        args.push("--tls-ca-cert".to_string());
        args.push(ca_cert.clone());
    }

    spawn_daemon(DaemonRole::Client, args)
}

/// Spawn a detached daemon process
fn spawn_daemon(role: DaemonRole, args: Vec<String>) -> wsproxy::Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| wsproxy::Error::config(format!("Failed to get current executable: {}", e)))?;

    // Pre-allocate the daemon ID
    let id = {
        let _lock = registry::FileLock::acquire()
            .map_err(|e| wsproxy::Error::config(format!("Failed to acquire lock: {}", e)))?;
        let daemons = registry::read();
        daemons.iter().map(|d| d.id).max().unwrap_or(0) + 1
    };

    // Spawn the daemon with its ID already set
    let mut cmd = Command::new(&exe);
    cmd.arg("daemon");
    cmd.args(&args);
    cmd.env(DAEMON_ENV_VAR, "1");
    cmd.env(DAEMON_ID_VAR, id.to_string());
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::inherit());

    let child = cmd
        .spawn()
        .map_err(|e| wsproxy::Error::config(format!("Failed to spawn daemon process: {}", e)))?;

    let pid = child.id();

    // Register in the registry with the actual PID
    {
        let _lock = registry::FileLock::acquire()
            .map_err(|e| wsproxy::Error::config(format!("Failed to acquire lock: {}", e)))?;
        let mut daemons = registry::read();

        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        daemons.push(registry::DaemonInfo {
            id,
            pid,
            role,
            args: args.clone(),
            started_at,
        });

        registry::write(&daemons)
            .map_err(|e| wsproxy::Error::config(format!("Failed to write registry: {}", e)))?;
    }

    eprintln!("Daemon started with ID {} (PID {})", id, pid);

    Ok(())
}

/// List all registered daemons, cleaning up dead ones
pub fn list() -> wsproxy::Result<Vec<DaemonInfo>> {
    let _lock = registry::FileLock::acquire()
        .map_err(|e| wsproxy::Error::config(format!("Failed to acquire lock: {}", e)))?;

    let daemons = registry::read();

    // Filter out dead processes
    let (alive, _dead): (Vec<_>, Vec<_>) = daemons
        .into_iter()
        .partition(|d| registry::is_process_alive(d.pid));

    // Always write back to clean up dead entries
    registry::write(&alive)
        .map_err(|e| wsproxy::Error::config(format!("Failed to write registry: {}", e)))?;

    Ok(alive)
}

/// Kill a daemon by ID
///
/// If `force` is false, sends SIGTERM to the restart loop, which causes
/// the worker to drain connections gracefully before exiting.
///
/// If `force` is true, sends SIGKILL to both the restart loop and worker
/// processes, immediately terminating all connections.
pub fn kill(id: u32, force: bool) -> wsproxy::Result<bool> {
    let _lock = registry::FileLock::acquire()
        .map_err(|e| wsproxy::Error::config(format!("Failed to acquire lock: {}", e)))?;

    let mut daemons = registry::read();

    if let Some(pos) = daemons.iter().position(|d| d.id == id) {
        let daemon = &daemons[pos];
        let killed = if force {
            registry::force_kill_process(daemon.pid)
        } else {
            registry::kill_process(daemon.pid)
        };

        if killed {
            daemons.remove(pos);
            registry::write(&daemons)
                .map_err(|e| wsproxy::Error::config(format!("Failed to write registry: {}", e)))?;
        }

        Ok(killed)
    } else {
        Ok(false)
    }
}

/// Wait for stdin to close (indicating parent daemon died).
/// Returns when EOF is detected on stdin.
pub async fn wait_for_stdin_close() {
    use tokio::io::AsyncReadExt;

    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 1];

    // This returns Ok(0) when stdin is closed (EOF)
    let _ = stdin.read(&mut buf).await;
}

/// Update the wsproxy binary and restart all daemons.
///
/// This function:
/// 1. Reads all running daemon info
/// 2. Stops each daemon's restart loop (workers continue serving)
/// 3. Replaces the current binary with the new one
/// 4. Spawns new daemons with the same arguments
///
/// Old workers with active connections will continue until they naturally close.
pub fn update(new_binary_path: &str) -> wsproxy::Result<()> {
    use std::fs;
    use std::path::Path;

    let new_binary = Path::new(new_binary_path);

    // Verify the new binary exists and is executable
    if !new_binary.exists() {
        return Err(wsproxy::Error::config(format!(
            "New binary not found: {}",
            new_binary_path
        )));
    }

    // Get the current executable path
    let current_exe = std::env::current_exe()
        .map_err(|e| wsproxy::Error::config(format!("Failed to get current executable: {}", e)))?;

    // Read all daemon info before stopping them
    let daemons = {
        let _lock = registry::FileLock::acquire()
            .map_err(|e| wsproxy::Error::config(format!("Failed to acquire lock: {}", e)))?;
        registry::read()
    };

    if daemons.is_empty() {
        eprintln!("No daemons running. Updating binary only.");
    } else {
        eprintln!(
            "Stopping {} daemon(s) for update (workers will drain)...",
            daemons.len()
        );

        // Stop all daemon restart loops (sending SIGTERM)
        // This will cause stdin to close for workers, triggering graceful drain
        for daemon in &daemons {
            registry::kill_process(daemon.pid);
        }
    }

    // Clear the registry since we killed all daemons
    {
        let _lock = registry::FileLock::acquire()
            .map_err(|e| wsproxy::Error::config(format!("Failed to acquire lock: {}", e)))?;
        registry::write(&[])
            .map_err(|e| wsproxy::Error::config(format!("Failed to clear registry: {}", e)))?;
    }

    // Replace the current binary with the new one
    eprintln!(
        "Updating binary: {} -> {}",
        new_binary_path,
        current_exe.display()
    );

    // We can't replace a running executable directly on most platforms,
    // so we rename the old one first, then copy the new one in place.

    // Try to clean up old backups first
    if let Some(parent) = current_exe.parent()
        && let Some(file_name) = current_exe.file_name()
    {
        let file_name = file_name.to_string_lossy();
        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&format!("{}.old.", file_name)) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    // Find a unique backup name with numbered extension
    let mut n = 1;
    let backup_path = loop {
        let mut name = current_exe.as_os_str().to_owned();
        name.push(format!(".old.{}", n));
        let candidate = std::path::PathBuf::from(name);
        if !candidate.exists() {
            break candidate;
        }
        n += 1;
    };

    // Rename current to backup
    fs::rename(&current_exe, &backup_path)
        .map_err(|e| wsproxy::Error::config(format!("Failed to backup current binary: {}", e)))?;

    // Copy new binary to current location
    fs::copy(new_binary, &current_exe)
        .map_err(|e| wsproxy::Error::config(format!("Failed to copy new binary: {}", e)))?;

    // Set executable permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&current_exe)
            .map_err(|e| wsproxy::Error::config(format!("Failed to get file metadata: {}", e)))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&current_exe, perms)
            .map_err(|e| wsproxy::Error::config(format!("Failed to set permissions: {}", e)))?;
    }

    eprintln!("Binary updated successfully.");

    // Restart all daemons with the same arguments
    for daemon in &daemons {
        eprintln!("Restarting daemon {} ({})...", daemon.id, daemon.role);
        spawn_daemon(daemon.role, daemon.args.clone())?;
    }

    if !daemons.is_empty() {
        eprintln!("All {} daemon(s) restarted with new binary.", daemons.len());
    }

    Ok(())
}
