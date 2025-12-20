use async_trait::async_trait;
use mpc_network::NetworkService;
use mpc_rpc_api::RpcResult;
use mpc_rpc_api::SystemApiServer;

pub struct System {
    network_service: NetworkService,
}

impl System {
    pub fn new(network_service: NetworkService) -> Self {
        Self { network_service }
    }
}

#[async_trait]
impl SystemApiServer for System {
    async fn local_peer_id(&self) -> RpcResult<String> {
        Ok(self.network_service.local_peer_id().to_base58())
    }

    async fn peers(&self) -> RpcResult<Vec<String>> {
        let peers = self
            .network_service
            .get_connected_peers()
            .await
            .into_iter()
            .map(|p| p.to_base58())
            .collect();
        Ok(peers)
    }
}
