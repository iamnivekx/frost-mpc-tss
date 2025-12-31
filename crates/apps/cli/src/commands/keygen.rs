use anyhow::anyhow;
use clap::Parser;
use mpc_rpc_api::new_client;
use mpc_rpc_api::TssApiClient;
use tracing::info;

#[derive(Parser, Debug)]
#[command(version, about, long_about = "Keygen command")]
pub struct Command {
    /// room identifier
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

impl Command {
    /// Execute `keygen` command
    pub async fn execute(self) -> anyhow::Result<()> {
        let pub_key = new_client(&self.address)
            .await?
            .keygen(self.room, self.parties, self.threshold)
            .await
            .map_err(|e| anyhow!("error keygen: {e}"))?;

        info!(
            "Keygen finished! curve: {:?}, public key => {:?}",
            pub_key.curve, pub_key.bytes
        );

        Ok(())
    }
}
