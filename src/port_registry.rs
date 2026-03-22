//! Port registry for daemon mode.
//!
//! This module provides functionality for workers to report their bound port
//! to the daemon registry, allowing `wsproxy daemon list` to show actual ports.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Environment variable containing the daemon ID
const DAEMON_ID_VAR: &str = "__WSPROXY_DAEMON_ID";

/// Environment variable to override the registry file path
const REGISTRY_FILE_ENV: &str = "WSPROXY_REGISTRY_FILE";

/// Minimal daemon info for JSON deserialization/serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonInfo {
    id: u32,
    #[serde(flatten)]
    rest: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
}

/// Get the path to the daemon registry file
fn registry_path() -> PathBuf {
    if let Ok(path) = std::env::var(REGISTRY_FILE_ENV) {
        return PathBuf::from(path);
    }
    let mut path = std::env::temp_dir();
    path.push("wsproxy");
    fs::create_dir_all(&path).ok();
    path.push("daemons.json");
    path
}

/// Lock file path
fn lock_path() -> PathBuf {
    let mut path = registry_path();
    path.set_extension("lock");
    path
}

/// Simple file-based lock
struct FileLock {
    _file: fs::File,
}

impl FileLock {
    fn acquire() -> std::io::Result<Self> {
        use std::time::{Duration, Instant};

        let lock_path = lock_path();
        let start = Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => return Ok(Self { _file: file }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Check if lock is stale (older than 30 seconds)
                    if let Ok(metadata) = fs::metadata(&lock_path)
                        && let Ok(modified) = metadata.modified()
                        && modified.elapsed().unwrap_or_default() > Duration::from_secs(30)
                    {
                        fs::remove_file(&lock_path).ok();
                        continue;
                    }
                    if start.elapsed() > Duration::from_secs(5) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "Timeout waiting for registry lock",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        fs::remove_file(lock_path()).ok();
    }
}

/// Report the bound port to the daemon registry.
///
/// This should be called by server/client after binding to update the
/// registry with the actual port. Only has an effect when running as
/// a daemon worker (when `__WSPROXY_DAEMON_ID` env var is set).
///
/// Errors are silently ignored since this is non-critical functionality.
pub fn report_port(port: u16) {
    let _ = try_report_port(port);
}

fn try_report_port(port: u16) -> std::io::Result<()> {
    // Get daemon ID from environment
    let id: u32 = std::env::var(DAEMON_ID_VAR)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "No daemon ID"))?;

    let _lock = FileLock::acquire()?;

    let path = registry_path();
    let content = fs::read_to_string(&path).unwrap_or_else(|_| "[]".to_string());
    let mut daemons: Vec<DaemonInfo> = serde_json::from_str(&content).unwrap_or_default();

    // Update the port for this daemon
    if let Some(daemon) = daemons.iter_mut().find(|d| d.id == id) {
        daemon.port = Some(port);
    }

    let content = serde_json::to_string_pretty(&daemons)?;
    fs::write(&path, content)?;

    Ok(())
}
