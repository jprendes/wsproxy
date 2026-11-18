//! Server configuration file support with hot-reload.
//!
//! # Example Configuration (TOML)
//!
//! ```toml
//! listen = "0.0.0.0:8080"
//! default_target = "127.0.0.1:22"
//!
//! [routes]
//! "/ssh" = "127.0.0.1:22"
//! "/db" = "127.0.0.1:5432"
//!
//! [tls]
//! cert = "cert.pem"
//! key = "key.pem"
//! # Or use: self_signed = true
//! ```

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use tokio::sync::{RwLock, mpsc};

use crate::error::{Error, Result};
use crate::router::Router;

/// TLS configuration in config file
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct TlsFileConfig {
    /// Path to TLS certificate file (PEM format)
    pub cert: Option<String>,
    /// Path to TLS private key file (PEM format)
    pub key: Option<String>,
    /// Generate a self-signed certificate
    pub self_signed: bool,
}

/// Server configuration loaded from a TOML file
#[derive(Debug, Clone, Deserialize)]
pub struct ServerFileConfig {
    /// Address to listen for WebSocket connections
    pub listen: String,
    /// Default target for paths that don't match any route
    pub default_target: Option<String>,
    /// Route mappings (path -> target address)
    #[serde(default)]
    pub routes: HashMap<String, String>,
    /// TLS configuration
    #[serde(default)]
    pub tls: TlsFileConfig,
    /// Seconds to retain disconnected resumable tunnels. Zero disables resumption.
    #[serde(default, rename = "reconnect_window", alias = "reconnect_window_secs")]
    pub reconnect_window_secs: u32,
    /// Reconnect windows in seconds, keyed by an exact configured route pattern.
    #[serde(default)]
    pub reconnect_windows: HashMap<String, u32>,
}

impl ServerFileConfig {
    /// Load configuration from a TOML file
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(|e| {
            Error::config(format!(
                "failed to read config file '{}': {}",
                path.as_ref().display(),
                e
            ))
        })?;
        Self::parse(&content)
    }

    /// Parse configuration from a TOML string
    pub fn parse(content: &str) -> Result<Self> {
        let config: ServerFileConfig =
            toml::from_str(content).map_err(|e| Error::config(format!("invalid config: {}", e)))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration
    fn validate(&self) -> Result<()> {
        for pattern in self.reconnect_windows.keys() {
            if !self.routes.contains_key(pattern) {
                return Err(Error::config(format!(
                    "reconnect_windows references unknown route '{}'",
                    pattern
                )));
            }
        }
        // Validate listen address
        resolve_addr(&self.listen).map_err(|e| {
            Error::config(format!("invalid listen address '{}': {}", self.listen, e))
        })?;

        // Validate TLS config
        if self.tls.self_signed && (self.tls.cert.is_some() || self.tls.key.is_some()) {
            return Err(Error::config("cannot use self_signed with cert/key files"));
        }
        if self.tls.cert.is_some() != self.tls.key.is_some() {
            return Err(Error::config("both cert and key must be provided"));
        }

        // Must have at least one route or default_target
        if self.routes.is_empty() && self.default_target.is_none() {
            return Err(Error::config(
                "at least one route or default_target is required",
            ));
        }

        Ok(())
    }

    /// Check if TLS is enabled
    pub fn has_tls(&self) -> bool {
        self.tls.self_signed || self.tls.cert.is_some()
    }

    /// Check if settings can be hot-reloaded (everything except listen/TLS).
    pub fn only_routing_changed(&self, other: &Self) -> bool {
        self.listen == other.listen && self.tls == other.tls
    }
}

/// Resolve an address string to a SocketAddr using DNS resolution if needed.
fn resolve_addr(addr: impl ToSocketAddrs) -> std::io::Result<SocketAddr> {
    addr.to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "could not resolve address")
    })
}

/// Resolved server configuration with listen address and routes.
/// Route targets are stored as strings and resolved at connection time.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub listen_addr: SocketAddr,
    pub default_target: Option<String>,
    pub router: Router,
    pub reconnect_window_secs: u32,
    pub reconnect_windows: HashMap<String, u32>,
}

impl ResolvedConfig {
    /// Create resolved config from file config.
    /// Only the listen address is resolved immediately; route targets are
    /// stored as strings and resolved when connections are established.
    pub fn from_file_config(config: &ServerFileConfig) -> Result<Self> {
        config.validate()?;
        let listen_addr = resolve_addr(&config.listen)
            .map_err(|e| Error::config(format!("invalid listen address: {}", e)))?;

        let mut router = Router::new();
        for (path, target) in &config.routes {
            router.insert(path, target)?;
        }

        Ok(Self {
            listen_addr,
            default_target: config.default_target.clone(),
            router,
            reconnect_window_secs: config.reconnect_window_secs,
            reconnect_windows: config.reconnect_windows.clone(),
        })
    }

    pub(crate) fn resolve(&self, path: &str) -> (Option<String>, u32) {
        match self.router.resolve_with_pattern(path) {
            Some((pattern, target)) => (
                Some(target),
                self.reconnect_windows
                    .get(pattern)
                    .copied()
                    .unwrap_or(self.reconnect_window_secs),
            ),
            None => (self.default_target.clone(), self.reconnect_window_secs),
        }
    }
}

/// Shared routing configuration that can be hot-reloaded
pub type SharedRoutingConfig = Arc<RwLock<ResolvedConfig>>;

/// Create a shared routing config from file config
pub fn create_shared_config(config: &ServerFileConfig) -> Result<SharedRoutingConfig> {
    let resolved = ResolvedConfig::from_file_config(config)?;
    Ok(Arc::new(RwLock::new(resolved)))
}

/// Configuration change notification
#[derive(Debug, Clone)]
pub enum ConfigChange {
    /// Routing or reconnect window changed - can hot-reload
    RoutingOnly(ServerFileConfig),
    /// Full restart required (listen address or TLS changed)
    FullRestart(ServerFileConfig),
    /// Error loading config
    Error(String),
}

/// Watch a configuration file for changes
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    receiver: mpsc::Receiver<ConfigChange>,
}

impl ConfigWatcher {
    /// Create a new config watcher for the given file
    pub fn new(config_path: impl AsRef<Path>, current_config: ServerFileConfig) -> Result<Self> {
        let path = config_path.as_ref().to_path_buf();
        let (tx, rx) = mpsc::channel(16);
        let current = Arc::new(std::sync::Mutex::new(current_config));

        let tx_clone = tx.clone();
        let path_clone = path.clone();
        let current_clone = Arc::clone(&current);

        let mut watcher = notify::recommended_watcher(move |res: std::result::Result<Event, _>| {
            if let Ok(event) = res {
                // Only react to modify/create events
                if !matches!(
                    event.kind,
                    notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                ) {
                    return;
                }

                // Check if our file was affected
                if !event.paths.iter().any(|p| p.ends_with(&path_clone)) {
                    return;
                }

                // Small delay to let the file finish writing
                std::thread::sleep(std::time::Duration::from_millis(50));

                // Try to load new config
                match ServerFileConfig::load(&path_clone) {
                    Ok(new_config) => {
                        let old_config = match current_clone.lock() {
                            Ok(guard) => guard,
                            Err(e) => {
                                let _ = tx_clone.blocking_send(ConfigChange::Error(format!(
                                    "mutex poisoned: {}",
                                    e
                                )));
                                return;
                            }
                        };
                        let change = if old_config.only_routing_changed(&new_config) {
                            ConfigChange::RoutingOnly(new_config.clone())
                        } else {
                            ConfigChange::FullRestart(new_config.clone())
                        };
                        drop(old_config);

                        // Update current config
                        match current_clone.lock() {
                            Ok(mut guard) => *guard = new_config,
                            Err(e) => {
                                let _ = tx_clone.blocking_send(ConfigChange::Error(format!(
                                    "mutex poisoned: {}",
                                    e
                                )));
                                return;
                            }
                        }

                        // Send notification (ignore if channel is full)
                        let _ = tx_clone.blocking_send(change);
                    }
                    Err(e) => {
                        let _ = tx_clone.blocking_send(ConfigChange::Error(e.to_string()));
                    }
                }
            }
        })
        .map_err(|e| Error::config(format!("failed to create file watcher: {}", e)))?;

        // Watch the parent directory (some editors replace files)
        let watch_path = path.parent().unwrap_or(&path);
        watcher
            .watch(watch_path, RecursiveMode::NonRecursive)
            .map_err(|e| Error::config(format!("failed to watch config file: {}", e)))?;

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
        })
    }

    /// Receive the next configuration change
    pub async fn recv(&mut self) -> Option<ConfigChange> {
        self.receiver.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_windows_follow_matched_patterns_and_global_fallback() -> Result<()> {
        let config = ServerFileConfig::parse(
            r#"
            listen = "127.0.0.1:8080"
            default_target = "localhost:22"
            reconnect_window = 600
            [routes]
            "/ssh/{host}" = "{host}:22"
            "/ssh/fixed" = "fixed:22"
            "/files/{*path}" = "files:80"
            "/raw" = "localhost:80"
            [reconnect_windows]
            "/ssh/{host}" = 1200
            "/files/{*path}" = 30
            "/raw" = 0
            "#,
        )?;
        let resolved = ResolvedConfig::from_file_config(&config)?;
        assert_eq!(
            resolved.resolve("/ssh/example/"),
            (Some("example:22".into()), 1200)
        );
        assert_eq!(
            resolved.resolve("/ssh/fixed"),
            (Some("fixed:22".into()), 600)
        );
        assert_eq!(resolved.resolve("/files/a/b").1, 30);
        assert_eq!(resolved.resolve("/raw").1, 0);
        assert_eq!(
            resolved.resolve("/unmatched"),
            (Some("localhost:22".into()), 600)
        );
        let mut changed = config.clone();
        changed.reconnect_windows.insert("/raw".into(), 10);
        assert!(config.only_routing_changed(&changed));
        let existing = resolved;
        assert_eq!(
            ResolvedConfig::from_file_config(&changed)?
                .resolve("/raw")
                .1,
            10
        );
        assert_eq!(existing.resolve("/raw").1, 0);
        Ok(())
    }

    #[test]
    fn reconnect_window_aliases_and_invalid_overrides() -> Result<()> {
        let base = "listen = '127.0.0.1:8080'\ndefault_target = 'localhost:22'\n";
        for key in ["reconnect_window", "reconnect_window_secs"] {
            assert_eq!(
                ServerFileConfig::parse(&format!("{base}{key} = 600"))?.reconnect_window_secs,
                600
            );
        }
        assert!(
            ServerFileConfig::parse(&format!(
                "{base}reconnect_window = 10\nreconnect_window_secs = 20"
            ))
            .is_err()
        );
        assert!(
            ServerFileConfig::parse(&format!("{base}[reconnect_windows]\n'/missing' = 30"))
                .is_err()
        );
        for value in ["-1", "1.5", "4294967296", "'unlimited'"] {
            assert!(
                ServerFileConfig::parse(&format!(
                    "{base}[routes]\n'/ssh' = 'localhost:22'\n[reconnect_windows]\n'/ssh' = {value}"
                ))
                .is_err()
            );
        }
        let enabled = ServerFileConfig::parse(&format!(
            "{base}[routes]\n'/ssh' = 'localhost:22'\n[reconnect_windows]\n'/ssh' = 60"
        ))?;
        let resolved = ResolvedConfig::from_file_config(&enabled)?;
        assert_eq!(resolved.resolve("/ssh").1, 60);
        assert_eq!(resolved.resolve("/other").1, 0);
        Ok(())
    }

    #[test]
    fn reconnect_window_is_optional_validated_and_hot_reloadable() -> Result<()> {
        let base = "listen = '127.0.0.1:8080'\ndefault_target = '127.0.0.1:22'\n";
        let disabled = ServerFileConfig::parse(base)?;
        assert_eq!(disabled.reconnect_window_secs, 0);
        let enabled = ServerFileConfig::parse(&format!("{base}reconnect_window_secs = 120"))?;
        assert_eq!(
            ResolvedConfig::from_file_config(&enabled)?.reconnect_window_secs,
            120
        );
        assert!(disabled.only_routing_changed(&enabled));
        for value in ["-1", "1.5", "4294967296", "'unlimited'"] {
            assert!(
                ServerFileConfig::parse(&format!("{base}reconnect_window_secs = {value}")).is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn test_parse_minimal_config() -> Result<()> {
        let config = ServerFileConfig::parse(
            r#"
            listen = "0.0.0.0:8080"
            default_target = "127.0.0.1:22"
            "#,
        )?;

        assert_eq!(config.listen, "0.0.0.0:8080");
        assert_eq!(config.default_target, Some("127.0.0.1:22".to_string()));
        assert!(config.routes.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_full_config() -> Result<()> {
        let config = ServerFileConfig::parse(
            r#"
            listen = "0.0.0.0:8443"
            default_target = "127.0.0.1:22"

            [routes]
            "/ssh" = "127.0.0.1:22"
            "/db" = "127.0.0.1:5432"

            [tls]
            cert = "cert.pem"
            key = "key.pem"
            "#,
        )?;

        assert_eq!(config.listen, "0.0.0.0:8443");
        assert_eq!(config.routes.get("/ssh"), Some(&"127.0.0.1:22".to_string()));
        assert_eq!(
            config.routes.get("/db"),
            Some(&"127.0.0.1:5432".to_string())
        );
        assert!(config.has_tls());
        Ok(())
    }

    #[test]
    fn test_parse_self_signed_tls() -> Result<()> {
        let config = ServerFileConfig::parse(
            r#"
            listen = "0.0.0.0:8443"
            default_target = "127.0.0.1:22"

            [tls]
            self_signed = true
            "#,
        )?;

        assert!(config.tls.self_signed);
        assert!(config.has_tls());
        Ok(())
    }

    #[test]
    fn test_invalid_listen_address() {
        let result = ServerFileConfig::parse(
            r#"
            listen = "invalid"
            default_target = "127.0.0.1:22"
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_no_routes_or_default() {
        let result = ServerFileConfig::parse(
            r#"
            listen = "0.0.0.0:8080"
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_conflicting_tls_config() {
        let result = ServerFileConfig::parse(
            r#"
            listen = "0.0.0.0:8080"
            default_target = "127.0.0.1:22"

            [tls]
            cert = "cert.pem"
            key = "key.pem"
            self_signed = true
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_only_routing_changed() -> Result<()> {
        let config1 = ServerFileConfig::parse(
            r#"
            listen = "0.0.0.0:8080"
            default_target = "127.0.0.1:22"
            "#,
        )?;

        let config2 = ServerFileConfig::parse(
            r#"
            listen = "0.0.0.0:8080"
            default_target = "127.0.0.1:23"
            "#,
        )?;

        let config3 = ServerFileConfig::parse(
            r#"
            listen = "0.0.0.0:9090"
            default_target = "127.0.0.1:22"
            "#,
        )?;

        assert!(config1.only_routing_changed(&config2));
        assert!(!config1.only_routing_changed(&config3));
        Ok(())
    }
}
