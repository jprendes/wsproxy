//! Daemon registry for tracking running daemon processes.
//!
//! Provides cross-platform storage and management of daemon process information.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Role of the daemon (server or client)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DaemonRole {
    Server,
    Client,
}

impl std::fmt::Display for DaemonRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonRole::Server => write!(f, "server"),
            DaemonRole::Client => write!(f, "client"),
        }
    }
}

/// Information about a registered daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub id: u32,
    pub pid: u32,
    pub role: DaemonRole,
    pub args: Vec<String>,
    pub started_at: u64, // Unix timestamp
}

/// Get the path to the daemon registry file
fn registry_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push("wsproxy");
    fs::create_dir_all(&path).ok();
    path.push("daemons.json");
    path
}

/// Lock file for atomic registry access
fn lock_path() -> PathBuf {
    let mut path = registry_path();
    path.set_extension("lock");
    path
}

/// Simple file-based lock for cross-platform compatibility
pub(crate) struct FileLock {
    _file: fs::File,
}

impl FileLock {
    pub fn acquire() -> std::io::Result<Self> {
        let lock_path = lock_path();
        let start = std::time::Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => return Ok(Self { _file: file }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Check if lock is stale (older than 30 seconds)
                    if let Ok(metadata) = fs::metadata(&lock_path) {
                        if let Ok(modified) = metadata.modified() {
                            if modified.elapsed().unwrap_or_default() > Duration::from_secs(30) {
                                fs::remove_file(&lock_path).ok();
                                continue;
                            }
                        }
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

/// Read the daemon registry
pub(crate) fn read() -> Vec<DaemonInfo> {
    let path = registry_path();
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Write the daemon registry
pub(crate) fn write(daemons: &[DaemonInfo]) -> std::io::Result<()> {
    let path = registry_path();
    let content = serde_json::to_string_pretty(daemons)?;
    fs::write(&path, content)
}

/// Unregister a daemon from the registry
pub(crate) fn unregister(id: u32) -> std::io::Result<()> {
    let _lock = FileLock::acquire()?;
    let mut daemons = read();
    daemons.retain(|d| d.id != id);
    write(&daemons)
}

/// Check if a process is still running (cross-platform)
pub(crate) fn is_process_alive(pid: u32) -> bool {
    use sysinfo::{Pid, System};

    let s = System::new_all();
    s.process(Pid::from_u32(pid)).is_some()
}

/// Kill a process by PID (cross-platform)
pub(crate) fn kill_process(pid: u32) -> bool {
    use sysinfo::{Pid, Signal, System};

    let s = System::new_all();
    s.process(Pid::from_u32(pid))
        .map(|p| p.kill_with(Signal::Term).unwrap_or(false))
        .unwrap_or(false)
}
