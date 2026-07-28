use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::net::{TcpListener, UdpSocket};

use doom_server::RoomName;

#[derive(Debug, Parser)]
#[command(name = "doomd", about = "DOOM multiplayer room server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve named rooms over WebSockets at /ws/{room} and UDP listeners.
    Serve {
        /// Address on which to listen for WebSocket clients.
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: SocketAddr,

        /// Maximum queued packets per connection.
        #[arg(long, default_value = "64")]
        outbox_capacity: NonZeroUsize,

        /// UDP listener pinned to one room, repeatable as ROOM=ADDR.
        /// `/ws/{room}` joins the same shared room.
        #[arg(long = "udp", value_name = "ROOM=ADDR")]
        udp: Vec<UdpSpec>,
    },
}

/// One validated `--udp ROOM=ADDR` specification.
#[derive(Debug, Clone)]
struct UdpSpec {
    room: RoomName,
    addr: SocketAddr,
}

impl FromStr for UdpSpec {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (room, addr) = value
            .split_once('=')
            .ok_or_else(|| format!("expected ROOM=ADDR, got {value:?}"))?;
        let room = RoomName::try_from(room).map_err(|error| error.to_string())?;
        let addr = addr
            .parse::<SocketAddr>()
            .map_err(|error| format!("invalid UDP address {addr:?}: {error}"))?;
        Ok(Self { room, addr })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve {
            listen,
            outbox_capacity,
            udp,
        } => {
            // Every specification is validated and every conflict
            // rejected before anything is bound.
            let mut seen = std::collections::HashSet::new();
            for spec in &udp {
                if spec.addr == listen {
                    bail!("UDP bind {} conflicts with the WebSocket listen", spec.addr);
                }
                if !seen.insert(spec.addr) {
                    bail!("duplicate UDP bind address {}", spec.addr);
                }
            }

            let listener = TcpListener::bind(listen)
                .await
                .with_context(|| format!("failed to bind {listen}"))?;
            let local_address = listener
                .local_addr()
                .context("failed to read bound address")?;
            eprintln!("doomd listening at ws://{local_address}/ws/<room>");

            let mut udp_listeners = Vec::new();
            for spec in udp {
                let socket = UdpSocket::bind(spec.addr)
                    .await
                    .with_context(|| format!("failed to bind UDP {}", spec.addr))?;
                let local = socket
                    .local_addr()
                    .context("failed to read bound UDP address")?;
                eprintln!("doomd listening at udp://{local} room {}", spec.room);
                udp_listeners.push((spec.room, socket));
            }

            doom_server::serve(listener, udp_listeners, outbox_capacity)
                .await
                .context("server failed")?;
        }
    }

    Ok(())
}
