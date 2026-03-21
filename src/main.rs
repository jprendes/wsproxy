use clap::{Parser, Subcommand};
use wsproxy::{ProxyClient, ProxyServer};

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Server {
            listen,
            route,
            default_target,
        } => {
            let mut builder = ProxyServer::builder();

            // Add routes
            for r in route {
                let (path, target) = r
                    .split_once('=')
                    .ok_or_else(|| format!("Invalid route format '{}', expected 'path=target'", r))?;
                builder = builder.route(path, target)?;
            }

            // Set default target if provided
            if let Some(target) = default_target {
                builder = builder.default_target(&target)?;
            }

            let server = builder.bind(&listen)?;

            eprintln!("Proxy server listening on {}", listen);
            server.run().await?;
        }

        Commands::Client { listen, server } => {
            let client = ProxyClient::bind(&listen, &server)?;

            eprintln!("Proxy client listening on {}, forwarding to {}", listen, server);
            client.run().await?;
        }
    }

    Ok(())
}
