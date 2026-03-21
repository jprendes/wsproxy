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
        /// Address to listen for WebSocket connections (e.g., "0.0.0.0:8080")
        #[arg(short, long)]
        listen: String,

        /// Route mapping in the format "path=target" (e.g., "/ssh=127.0.0.1:22")
        /// Can be specified multiple times
        #[arg(short, long, value_name = "PATH=TARGET")]
        route: Vec<String>,

        /// Default target for paths that don't match any route (e.g., "127.0.0.1:22")
        #[arg(short, long)]
        default_target: Option<String>,
    },

    /// Run the proxy client (TCP -> WebSocket)
    Client {
        /// Address to listen for TCP connections (e.g., "127.0.0.1:2222")
        #[arg(short, long)]
        listen: String,

        /// WebSocket server URL to connect to (e.g., "ws://server:8080/ssh")
        #[arg(short, long)]
        server: String,
    },

    /// Run the proxy server as a daemon with automatic restart
    Daemon {
        /// Address to listen for WebSocket connections (e.g., "0.0.0.0:8080")
        #[arg(short, long)]
        listen: String,

        /// Route mapping in the format "path=target" (e.g., "/ssh=127.0.0.1:22")
        /// Can be specified multiple times
        #[arg(short, long, value_name = "PATH=TARGET")]
        route: Vec<String>,

        /// Default target for paths that don't match any route (e.g., "127.0.0.1:22")
        #[arg(short, long)]
        default_target: Option<String>,
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
            listen,
            route,
            default_target,
        } => {
            // Check if we should monitor stdin for parent death (daemon mode)
            if daemon::should_monitor_stdin() {
                tokio::select! {
                    result = wsproxy::server::run(&listen, &route, default_target.as_deref()) => {
                        result?;
                    }
                    _ = daemon::wait_for_stdin_close() => {
                        eprintln!("Parent daemon died, shutting down server");
                        std::process::exit(0);
                    }
                }
            } else {
                wsproxy::server::run(&listen, &route, default_target.as_deref()).await?;
            }
        }

        Commands::Client { listen, server } => {
            wsproxy::client::run(&listen, &server).await?;
        }

        Commands::Daemon {
            listen,
            route,
            default_target,
        } => {
            daemon::spawn(listen, route, default_target)?;
        }
    }

    Ok(())
}
