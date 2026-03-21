//! Daemon mode functionality for automatic server restart with exponential backoff.

use std::process::{Command, Stdio};
use std::time::Duration;

/// Environment variable used to indicate we're running as the daemon subprocess
const DAEMON_ENV_VAR: &str = "__WSPROXY_DAEMON_CHILD";

/// Environment variable to indicate the server should monitor stdin for parent death
const MONITOR_STDIN_VAR: &str = "__WSPROXY_MONITOR_STDIN";

/// Check if this process is running as the daemon child (restart loop)
pub fn is_daemon_child() -> bool {
    std::env::var(DAEMON_ENV_VAR).is_ok()
}

/// Check if the server should monitor stdin for parent death
pub fn should_monitor_stdin() -> bool {
    std::env::var(MONITOR_STDIN_VAR).is_ok()
}

/// Run the daemon restart loop with exponential backoff.
/// This function never returns - it continuously restarts the server.
pub fn run_restart_loop() -> ! {
    const MIN_BACKOFF_MS: u64 = 1;
    const MAX_BACKOFF_MS: u64 = 5 * 60 * 1000; // 5 minutes

    let args: Vec<String> = std::env::args().collect();

    // Convert daemon command to server command
    let server_args: Vec<String> = args
        .iter()
        .map(|arg| {
            if arg == "daemon" {
                "server".to_string()
            } else {
                arg.clone()
            }
        })
        .collect();

    let mut backoff_ms = MIN_BACKOFF_MS;

    loop {
        eprintln!("Starting wsproxy server...");

        // Use piped stdin - when this process dies, stdin closes,
        // and the server will detect EOF
        let mut child = match Command::new(&server_args[0])
            .args(&server_args[1..])
            .env_remove(DAEMON_ENV_VAR)
            .env(MONITOR_STDIN_VAR, "1")
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                eprintln!("Failed to start wsproxy server: {}", e);
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
                eprintln!("wsproxy server exited successfully");
            }
            Ok(status) => {
                eprintln!("wsproxy server exited with status: {}", status);
            }
            Err(e) => {
                eprintln!("Failed to wait for wsproxy server: {}", e);
            }
        }

        eprintln!("Restarting in {} ms...", backoff_ms);
        std::thread::sleep(Duration::from_millis(backoff_ms));

        // Exponential backoff with cap
        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
    }
}

/// Spawn a detached daemon process that will manage server restarts.
pub fn spawn(
    listen: String,
    route: Vec<String>,
    default_target: Option<String>,
) -> wsproxy::Result<()> {
    let exe = std::env::current_exe().map_err(|e| {
        wsproxy::Error::config(format!("Failed to get current executable: {}", e))
    })?;

    let mut cmd = Command::new(exe);
    cmd.arg("daemon");
    cmd.arg("--listen").arg(&listen);

    for r in &route {
        cmd.arg("--route").arg(r);
    }

    if let Some(target) = &default_target {
        cmd.arg("--default-target").arg(target);
    }

    // Set env var to indicate this is the daemon child
    cmd.env(DAEMON_ENV_VAR, "1");

    // Detach from parent
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::inherit()); // Keep stderr for logging

    let child = cmd.spawn().map_err(|e| {
        wsproxy::Error::config(format!("Failed to spawn daemon process: {}", e))
    })?;

    eprintln!("Daemon started with PID {}", child.id());

    Ok(())
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
