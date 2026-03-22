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
    /// Actual port the daemon is listening on (set after binding)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// Environment variable to override the registry file path
pub const REGISTRY_FILE_ENV: &str = "WSPROXY_REGISTRY_FILE";

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
    pub async fn acquire() -> std::io::Result<Self> {
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
                    tokio::time::sleep(Duration::from_millis(50)).await;
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
#[allow(dead_code)]
pub(crate) async fn unregister(id: u32) -> std::io::Result<()> {
    let _lock = FileLock::acquire().await?;
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

/// Send graceful shutdown signal to a process by PID (cross-platform)
///
/// Sends Ctrl+C (SIGINT) to trigger graceful shutdown with connection draining.
/// Returns true if signal was sent or process doesn't exist.
pub(crate) fn kill_process(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Send SIGINT to daemon - it will forward to worker and wait for graceful shutdown
        // SAFETY: This is the standard POSIX kill function
        let result = unsafe { libc::kill(pid as i32, libc::SIGINT) };
        // Success, or process doesn't exist (already dead)
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            AttachConsole, CTRL_C_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
            SetConsoleCtrlHandler,
        };

        // SAFETY: These are standard Windows console APIs
        // No need to save/restore state since this process exits immediately after
        unsafe {
            // Detach from our current console (if any) - required before AttachConsole
            FreeConsole();

            // Attach to the target process's console
            if AttachConsole(pid) == 0 {
                return false;
            }

            // Disable Ctrl-C handling so we don't kill ourselves
            SetConsoleCtrlHandler(None, 1);

            // Send Ctrl+C to all processes attached to the console
            GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) != 0
        }
    }
}

/// Force kill a process and all its children by PID (cross-platform)
/// Returns true if process was killed or doesn't exist.
pub(crate) fn force_kill_process(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Use sysinfo to find and kill child processes first
        use sysinfo::{Pid, System};

        let s = System::new_all();
        let target_pid = Pid::from_u32(pid);

        // Kill all child processes with SIGKILL
        for process in s.processes().values() {
            if process.parent() == Some(target_pid) {
                // SAFETY: This is the standard POSIX kill function
                unsafe { libc::kill(process.pid().as_u32() as i32, libc::SIGKILL) };
            }
        }

        // Then kill the parent with SIGKILL
        // SAFETY: This is the standard POSIX kill function
        let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        // Success, or process doesn't exist (already dead)
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[cfg(windows)]
    {
        use sysinfo::{Pid, System};
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };

        let s = System::new_all();
        let target_pid = Pid::from_u32(pid);

        // Helper to terminate a single process
        let terminate = |pid: u32| -> bool {
            // SAFETY: OpenProcess and TerminateProcess are standard Windows APIs
            unsafe {
                let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
                if handle.is_null() {
                    return false;
                }
                let result = TerminateProcess(handle, 1) != 0;
                CloseHandle(handle);
                result
            }
        };

        // Kill all child processes first
        for process in s.processes().values() {
            if process.parent() == Some(target_pid) {
                terminate(process.pid().as_u32());
            }
        }

        // Then kill the parent process
        terminate(pid)
    }
}
