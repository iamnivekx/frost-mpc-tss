use anyhow::anyhow;
use jsonrpsee::server::{ServerBuilder, ServerHandle};
use std::net::SocketAddr;

pub struct JsonRPCServer {
    handle: ServerHandle,
}

impl JsonRPCServer {
    pub async fn new<T>(
        config: Config,
        module: jsonrpsee::RpcModule<T>,
    ) -> Result<Self, anyhow::Error>
    where
        T: Send + Sync + 'static,
    {
        let addr: SocketAddr = config
            .host_address
            .parse()
            .map_err(|e| anyhow!("invalid rpc host_address '{}': {e}", config.host_address))?;

        let server = ServerBuilder::default()
            .build(addr)
            .await
            .map_err(|e| anyhow!("jsonrpsee server build failed: {e}"))?;

        let handle = server.start(module);
        Ok(Self { handle })
    }

    pub async fn run(self) -> Result<(), anyhow::Error> {
        self.handle.stopped().await;
        Ok(())
    }
}

pub struct Config {
    pub host_address: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host_address: "127.0.0.1:8080".to_string(),
        }
    }
}
