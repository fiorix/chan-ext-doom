use std::net::SocketAddr;
use std::num::NonZeroUsize;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;

use doom_server::websocket;

#[derive(Debug, Parser)]
#[command(name = "doomd", about = "DOOM multiplayer room server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve named rooms over WebSockets at /ws/{room}.
    Serve {
        /// Address on which to listen.
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: SocketAddr,

        /// Maximum queued packets per connection.
        #[arg(long, default_value = "64")]
        outbox_capacity: NonZeroUsize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve {
            listen,
            outbox_capacity,
        } => {
            let listener = TcpListener::bind(listen)
                .await
                .with_context(|| format!("failed to bind {listen}"))?;
            let local_address = listener
                .local_addr()
                .context("failed to read bound address")?;
            eprintln!("doomd listening at ws://{local_address}/ws/<room>");
            websocket::serve(listener, outbox_capacity)
                .await
                .context("WebSocket server failed")?;
        }
    }

    Ok(())
}
