use crate::tss::TssApiClient;
use anyhow::anyhow;
use jsonrpc_core_client::transports::ws;

pub async fn new_client(addr: String) -> Result<TssApiClient, anyhow::Error> {
    let url = addr.parse()?;
    let client = ws::connect::<TssApiClient>(&url)
        .await
        .map_err(|e| anyhow!("node connection terminated w/ err: {:?}", e))?;

    Ok(client)
}
