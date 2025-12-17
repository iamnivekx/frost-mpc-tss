use anyhow::anyhow;
use clap::Parser;
use mpc_rpc_api::new_client;
use mpc_rpc_api::TssApiClient;
use tracing::info;

#[derive(Parser, Debug)]
#[command(version, about, long_about = "Sign command")]
pub struct Command {
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

impl Command {
    /// Execute `sign` command
    pub async fn execute(self) -> anyhow::Result<()> {
        let res = new_client(&self.address)
            .await?
            .sign(self.room, self.threshold, self.messages.as_bytes().to_vec())
            .await
            .map_err(|e| anyhow!("error signing: {e}"))?;

        info!(
            "Signature: {:?}",
            serde_json::to_string(&res).map_err(|e| anyhow!("error encoding signature: {e}"))?
        );

        Ok(())
    }
}
