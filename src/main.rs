use clap::{Parser, Subcommand};

mod daemon;

#[derive(Parser)]
#[command(name = "wsproxy")]
#[command(about = "WebSocket proxy for TCP connections", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the proxy server (WebSocket -> TCP)
    Server {
        /// Path to configuration file (TOML format) with hot-reload support
        #[arg(short, long, conflicts_with_all = ["listen", "route", "default_target", "tls_cert", "tls_key", "tls_self_signed"])]
        config: Option<String>,

        /// Address to listen for WebSocket connections (e.g., "0.0.0.0:8080")
        #[arg(short, long, required_unless_present = "config")]
        listen: Option<String>,

        /// Route mapping in the format "path=target" (e.g., "/ssh=127.0.0.1:22")
        /// Can be specified multiple times
        #[arg(short, long, value_name = "PATH=TARGET")]
        route: Vec<String>,

        /// Default target for paths that don't match any route (e.g., "127.0.0.1:22")
        #[arg(short, long)]
        default_target: Option<String>,

        /// Path to TLS certificate file (PEM format) for WSS support
        #[arg(long, requires = "tls_key", conflicts_with = "tls_self_signed")]
        tls_cert: Option<String>,

        /// Path to TLS private key file (PEM format) for WSS support
        #[arg(long, requires = "tls_cert", conflicts_with = "tls_self_signed")]
        tls_key: Option<String>,

        /// Generate a self-signed TLS certificate for WSS support
        #[arg(long, conflicts_with_all = ["tls_cert", "tls_key"])]
        tls_self_signed: bool,
    },

    /// Run the proxy client (TCP -> WebSocket)
    Client {
        /// Address to listen for TCP connections (e.g., "127.0.0.1:2222")
        #[arg(short, long)]
        listen: String,

        /// WebSocket server URL to connect to (e.g., "ws://server:8080/ssh" or "wss://server:8080/ssh")
        #[arg(short, long)]
        server: String,

        /// Skip TLS certificate verification (insecure, for self-signed certificates)
        #[arg(short = 'k', long)]
        insecure: bool,

        /// Path to CA certificate file (PEM format) for verifying self-signed server certificates
        #[arg(long)]
        tls_ca_cert: Option<String>,
    },

    /// Run a single tunnel connection (stdin/stdout -> WebSocket)
    /// Useful for SSH ProxyCommand
    Tunnel {
        /// WebSocket server URL to connect to (e.g., "ws://server:8080/ssh" or "wss://server:8080/ssh")
        #[arg(short, long)]
        server: String,

        /// Skip TLS certificate verification (insecure, for self-signed certificates)
        #[arg(short = 'k', long)]
        insecure: bool,

        /// Path to CA certificate file (PEM format) for verifying self-signed server certificates
        #[arg(long)]
        tls_ca_cert: Option<String>,
    },

    /// Manage daemon processes with automatic restart
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Run the proxy server as a daemon with automatic restart
    Server {
        /// Path to configuration file (TOML format) with hot-reload support
        #[arg(short, long, conflicts_with_all = ["listen", "route", "default_target", "tls_cert", "tls_key", "tls_self_signed"])]
        config: Option<String>,

        /// Address to listen for WebSocket connections (e.g., "0.0.0.0:8080")
        #[arg(short, long, required_unless_present = "config")]
        listen: Option<String>,

        /// Route mapping in the format "path=target" (e.g., "/ssh=127.0.0.1:22")
        /// Can be specified multiple times
        #[arg(short, long, value_name = "PATH=TARGET")]
        route: Vec<String>,

        /// Default target for paths that don't match any route (e.g., "127.0.0.1:22")
        #[arg(short, long)]
        default_target: Option<String>,

        /// Path to TLS certificate file (PEM format) for WSS support
        #[arg(long, requires = "tls_key", conflicts_with = "tls_self_signed")]
        tls_cert: Option<String>,

        /// Path to TLS private key file (PEM format) for WSS support
        #[arg(long, requires = "tls_cert", conflicts_with = "tls_self_signed")]
        tls_key: Option<String>,

        /// Generate a self-signed TLS certificate for WSS support
        #[arg(long, conflicts_with_all = ["tls_cert", "tls_key"])]
        tls_self_signed: bool,
    },

    /// Run the proxy client as a daemon with automatic restart
    Client {
        /// Address to listen for TCP connections (e.g., "127.0.0.1:2222")
        #[arg(short, long)]
        listen: String,

        /// WebSocket server URL to connect to (e.g., "ws://server:8080/ssh" or "wss://server:8080/ssh")
        #[arg(short, long)]
        server: String,

        /// Skip TLS certificate verification (insecure, for self-signed certificates)
        #[arg(short = 'k', long)]
        insecure: bool,

        /// Path to CA certificate file (PEM format) for verifying self-signed server certificates
        #[arg(long)]
        tls_ca_cert: Option<String>,
    },

    /// List all running daemons
    List,

    /// Kill a daemon by ID
    Kill {
        /// The daemon ID to kill (from `daemon list`)
        id: u32,

        /// Force immediate shutdown without draining connections
        #[arg(short, long)]
        force: bool,
    },

    /// Update wsproxy binary with graceful connection draining
    ///
    /// This command:
    /// 1. Stops all daemon listeners from accepting new connections
    /// 2. Replaces the current wsproxy binary with the new one
    /// 3. Restarts all daemons with the new binary
    ///
    /// Existing connections continue to be served by the old binary
    /// until they naturally close.
    Update {
        /// Path to the new wsproxy binary
        path: String,
    },
}

fn main() {
    // Check if we're running as the daemon child process
    if daemon::is_daemon_child() {
        daemon::run_restart_loop();
    }

    if let Err(e) = run() {
        eprintln!("Error: {e:?}");
        std::process::exit(1);
    }
}

#[tokio::main]
async fn run() -> wsproxy::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Server {
            config,
            listen,
            route,
            default_target,
            tls_cert,
            tls_key,
            tls_self_signed,
        } => {
            // Config file mode with hot-reload
            if let Some(config_path) = config {
                if daemon::should_monitor_stdin() {
                    wsproxy::server::run_with_config_until_shutdown(
                        &config_path,
                        daemon::wait_for_stdin_close(),
                        std::time::Duration::from_secs(30),
                    )
                    .await?;
                } else {
                    wsproxy::server::run_with_config(&config_path).await?;
                }
            } else {
                // CLI mode
                let listen = listen.expect("listen is required when not using config");
                let tls = match (tls_cert, tls_key, tls_self_signed) {
                    (Some(cert), Some(key), false) => wsproxy::server::TlsMode::Files {
                        cert: cert.leak(),
                        key: key.leak(),
                    },
                    (None, None, true) => wsproxy::server::TlsMode::SelfSigned,
                    _ => wsproxy::server::TlsMode::None,
                };

                if daemon::should_monitor_stdin() {
                    wsproxy::server::run_until_shutdown(
                        &listen,
                        &route,
                        default_target.as_deref(),
                        tls,
                        daemon::wait_for_stdin_close(),
                        std::time::Duration::from_secs(30),
                    )
                    .await?;
                } else {
                    wsproxy::server::run(&listen, &route, default_target.as_deref(), tls).await?;
                }
            }
        }

        Commands::Client {
            listen,
            server,
            insecure,
            tls_ca_cert,
        } => {
            let tls_options = wsproxy::client::TlsOptions {
                insecure,
                ca_cert_path: tls_ca_cert,
            };

            // Check if we should monitor stdin for parent death (daemon mode)
            if daemon::should_monitor_stdin() {
                wsproxy::client::run_until_shutdown(
                    &listen,
                    &server,
                    &tls_options,
                    daemon::wait_for_stdin_close(),
                    std::time::Duration::from_secs(30),
                )
                .await?;
            } else {
                wsproxy::client::run(&listen, &server, &tls_options).await?;
            }
        }

        Commands::Tunnel {
            server,
            insecure,
            tls_ca_cert,
        } => {
            let tls_options = wsproxy::client::TlsOptions {
                insecure,
                ca_cert_path: tls_ca_cert,
            };

            wsproxy::client::tunnel(&server, &tls_options).await?;
        }

        Commands::Daemon { action } => match action {
            DaemonAction::Server {
                config,
                listen,
                route,
                default_target,
                tls_cert,
                tls_key,
                tls_self_signed,
            } => {
                daemon::spawn_server(
                    config,
                    listen,
                    route,
                    default_target,
                    tls_cert,
                    tls_key,
                    tls_self_signed,
                )?;
            }

            DaemonAction::Client {
                listen,
                server,
                insecure,
                tls_ca_cert,
            } => {
                daemon::spawn_client(listen, server, insecure, tls_ca_cert)?;
            }

            DaemonAction::List => {
                let daemons = daemon::list()?;
                if daemons.is_empty() {
                    println!("No daemons running");
                } else {
                    println!("{:<4} {:<8} ARGUMENTS", "ID", "PID");
                    println!("{}", "-".repeat(50));
                    for d in daemons {
                        println!("{:<4} {:<8} {}", d.id, d.pid, d.args.join(" "));
                    }
                }
            }

            DaemonAction::Kill { id, force } => {
                if daemon::kill(id, force)? {
                    if force {
                        println!("Daemon {} force killed", id);
                    } else {
                        println!("Daemon {} killed (draining connections)", id);
                    }
                } else {
                    eprintln!("Daemon {} not found or could not be killed", id);
                    std::process::exit(1);
                }
            }

            DaemonAction::Update { path } => {
                daemon::update(&path)?;
            }
        },
    }

    Ok(())
}
