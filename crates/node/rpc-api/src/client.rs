use anyhow::anyhow;
use jsonrpsee::ws_client::WsClientBuilder;

pub async fn new_client(addr: &str) -> Result<jsonrpsee::ws_client::WsClient, anyhow::Error> {
    let client = WsClientBuilder::new()
        .build(addr)
        .await
        .map_err(|e| anyhow!("node connection terminated w/ err: {e}"))?;

    Ok(client)
}
