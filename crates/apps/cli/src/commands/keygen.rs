use anyhow::anyhow;
use clap::Parser;
use mpc_rpc_api::new_client;
use mpc_rpc_api::TssApiClient;
use tracing::info;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(version, about, long_about = "Keygen command")]
pub struct Command {
    /// Wallet identifier. If omitted, a random wallet id is generated.
    #[arg(short = 'r', long = "wallet-id", alias = "room")]
    pub wallet_id: Option<String>,

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
        let wallet_id = resolve_wallet_id(self.wallet_id);
        let pub_key = new_client(&self.address)
            .await?
            .keygen(wallet_id.clone(), self.parties, self.threshold)
            .await
            .map_err(|e| anyhow!("error keygen: {e}"))?;

        println!("wallet_id: {}", wallet_id);
        info!(
            "Keygen finished! curve: {:?}, public key => {:?}",
            pub_key.curve, pub_key.bytes
        );

        Ok(())
    }
}

fn resolve_wallet_id(wallet_id: Option<String>) -> String {
    match wallet_id {
        Some(id) if !id.trim().is_empty() => id,
        _ => Uuid::new_v4().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_wallet_id;

    #[test]
    fn resolve_wallet_id_keeps_user_value() {
        let id = resolve_wallet_id(Some("wallet-001".to_owned()));
        assert_eq!(id, "wallet-001");
    }

    #[test]
    fn resolve_wallet_id_generates_when_missing() {
        let id = resolve_wallet_id(None);
        assert!(!id.is_empty());
    }
}
