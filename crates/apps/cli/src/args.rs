use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "mpc-cli")]
#[command(about = "MPC Threshold Signature Scheme CLI")]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand, Clone)]
pub enum Command {
    /// Run MPC node
    Node(NodeArgs),

    /// Keygen args
    Keygen(KeygenArgs),

    /// Sign args
    Sign(SignArgs),

    /// Setup args
    Setup(SetupArgs),
}

#[derive(Debug, Parser, Clone)]
#[command(about = "Deploy MPC daemon")]
pub struct NodeArgs {
    /// Path to parties config
    #[arg(long)]
    pub config_path: String,

    /// Path to setup directory (where secret key saved)
    #[arg(long, default_value = "./data/:id/")]
    pub path: String,

    /// Peer discovery with Kad-DHT
    #[arg(long)]
    pub kademlia: bool,

    /// Peer discovery with mdns
    #[arg(long)]
    pub mdns: bool,
}

#[derive(Debug, Parser, Clone)]
#[command(about = "Setup MPC node")]
pub struct SetupArgs {
    /// libp2p multi address
    #[arg(long, default_value = "/ip4/127.0.0.1/tcp/4000")]
    pub multiaddr: String,

    /// RPC address
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub rpc_address: String,

    /// Path to configuration
    #[arg(long, default_value = "./config.json")]
    pub config_path: String,

    /// Path to setup artifacts
    #[arg(long, default_value = "./data/:id/")]
    pub path: String,
}

#[derive(Debug, Parser, Clone)]
#[command(about = "Generate threshold key")]
pub struct KeygenArgs {
    /// MPC room identifier
    #[arg(short = 'r', long = "room")]
    pub room: String,

    /// RPC server address
    #[arg(short = 'a', long = "address")]
    pub address: String,

    /// Threshold
    #[arg(short = 't', long = "threshold")]
    pub threshold: u16,

    /// Total number of parties
    #[arg(short = 'n', long = "parties")]
    pub parties: u16,
}

#[derive(Debug, Parser, Clone)]
#[command(about = "Sign a message")]
pub struct SignArgs {
    /// MPC room identifier
    #[arg(short = 'r', long = "room")]
    pub room: String,

    /// RPC server address
    #[arg(short = 'a', long = "address")]
    pub address: String,

    /// Threshold needed to sign the messages (T)
    #[arg(long)]
    pub threshold: u16,

    /// Messages to sign
    #[arg(long)]
    pub messages: String,
}
