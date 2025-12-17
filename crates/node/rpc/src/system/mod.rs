use async_trait::async_trait;
use mpc_rpc_api::RpcResult;
use mpc_rpc_api::SystemApiServer;

pub struct System;

impl System {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SystemApiServer for System {
    async fn local_peer_id(&self) -> RpcResult<String> {
        unimplemented!()
    }

    async fn peers(&self) -> RpcResult<Vec<String>> {
        unimplemented!()
    }
}
