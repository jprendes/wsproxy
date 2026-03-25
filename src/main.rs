use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

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
}

/// Wait for Ctrl+C (SIGINT) to trigger graceful shutdown.
/// This works on both Unix and Windows.
async fn wait_for_shutdown() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        eprintln!("Failed to listen for Ctrl+C: {e}");
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e:?}");
        std::process::exit(1);
    }
}

#[tokio::main]
async fn run() -> Result<()> {
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
                wsproxy::server::run_with_config_until_shutdown(
                    &config_path,
                    wait_for_shutdown(),
                    std::time::Duration::from_secs(30),
                )
                .await?;
            } else {
                // CLI mode
                let listen = listen.context("listen is required when not using config")?;
                let tls = match (tls_cert, tls_key, tls_self_signed) {
                    (Some(cert), Some(key), false) => wsproxy::server::TlsMode::Files {
                        cert: cert.leak(),
                        key: key.leak(),
                    },
                    (None, None, true) => wsproxy::server::TlsMode::SelfSigned,
                    _ => wsproxy::server::TlsMode::None,
                };

                wsproxy::server::run_until_shutdown(
                    &listen,
                    &route,
                    default_target.as_deref(),
                    tls,
                    wait_for_shutdown(),
                    std::time::Duration::from_secs(30),
                )
                .await?;
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

            wsproxy::client::run_until_shutdown(
                &listen,
                &server,
                &tls_options,
                wait_for_shutdown(),
                std::time::Duration::from_secs(30),
            )
            .await?;
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
    }

    Ok(())
}
