use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use tracing_otel_extra::Logger;

use crate::commands::{keygen, node, setup, sign};

#[derive(Parser)]
#[clap(version, about, propagate_version = true)]
pub struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

impl Cli {
    pub fn from_env_and_args() -> Self {
        dotenv().ok();
        Self::parse()
    }
}

/// Work seamlessly with Sola from the command line.
///
/// See `sola --help` for more information.
#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(name = "keygen", about = "Generate a threshold key")]
    Keygen(keygen::Command),
    #[command(name = "node", about = "Run MPC node")]
    Node(node::Command),
    #[command(name = "setup", about = "Setup MPC node")]
    Setup(setup::Command),
    #[command(name = "sign", about = "Sign a message")]
    Sign(sign::Command),
}

/// Parse CLI options, set up logging and run the chosen command.
pub async fn run() -> anyhow::Result<()> {
    let opt = Cli::from_env_and_args();
    let guard: tracing_otel_extra::OtelGuard = Logger::from_env(None)?
        .init()
        .expect("Failed to initialize logging");

    match opt.command {
        Commands::Keygen(command) => command.execute().await?,
        Commands::Node(command) => command.execute().await?,
        Commands::Setup(command) => command.execute().await?,
        Commands::Sign(command) => command.execute().await?,
    }
    drop(guard);
    Ok(())
}
