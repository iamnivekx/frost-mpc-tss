use clap::Parser;
use mpc_tss::generate_config;

#[derive(Parser, Debug)]
#[command(version, about, long_about = "Setup command")]
pub struct Command {
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

impl Command {
    /// Execute `setup` command
    pub async fn execute(self) -> anyhow::Result<()> {
        generate_config(
            self.config_path,
            self.path,
            self.multiaddr,
            self.rpc_address,
        )
        .map_err(|e| anyhow::anyhow!("error generating config: {e}"))?;

        Ok(())
    }
}
