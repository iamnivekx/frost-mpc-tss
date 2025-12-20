use anyhow::anyhow;
use clap::Parser;
use mpc_network::{Curve, NetworkWorker, NodeKeyConfig, Params, RoomConfig, Secret};
use mpc_rpc::{System, Tss};
use mpc_rpc_api::server::JsonRPCServer;
use mpc_rpc_api::{SystemApiServer, TssApiServer};
use mpc_runtime::{new_worker_and_service, LocalStorage};
use mpc_tss::{Config, TssFactory};
use std::iter;
use std::path::Path;
use tokio::task;
use tracing::info;

#[derive(Debug, Parser, Clone)]
#[command(about = "Run MPC node")]
pub struct Command {
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

impl Command {
    /// Execute `node` command
    pub async fn execute(self) -> anyhow::Result<()> {
        let config = Config::load_config(&self.config_path)?;
        let local_party = config.local.clone();
        let local_peer_id = local_party.network_peer.peer_id;
        let path_str = self
            .path
            .to_string()
            .replace(":id", &local_peer_id.to_base58());
        let base_path = Path::new(&path_str);
        let node_key = NodeKeyConfig::Ed25519(Secret::File(base_path.join("secret.key")));

        let boot_nodes: Vec<_> = config.boot_nodes.to_vec();

        let (room_id, room_cfg, room_rx) = RoomConfig::new_full(
            "tss/0".to_string(),
            boot_nodes.clone().into_iter(),
            config.boot_nodes.len(),
        );

        let params = Params {
            listen_address: local_party.network_peer.multiaddr.clone(),
            rooms: vec![room_cfg],
            mdns: self.mdns,
            kademlia: self.kademlia,
            boot_nodes: boot_nodes.clone().into_iter().collect(),
        };

        let net_worker = NetworkWorker::new(node_key, params)?;
        let net_service = net_worker.service().clone();

        let net_task = task::spawn(async {
            net_worker.run().await;
        });

        let local_peer_id = net_service.local_peer_id();
        let (rt_worker, rt_service) = new_worker_and_service(
            net_service.clone(),
            iter::once((room_id, room_rx)),
            TssFactory::new(
                format!("data/{}/key.share", local_peer_id.to_base58()),
                Curve::Ed25519,
            ),
            LocalStorage::new(base_path.join("peerset"), local_peer_id),
        );

        let rt_task = task::spawn(async {
            rt_worker.run().await;
        });

        let rpc_server = {
            let tss = Tss::new(rt_service);
            let system = System::new(net_service);
            let mut module = tss.into_rpc();
            module
                .merge(system.into_rpc())
                .map_err(|e| anyhow!("failed to merge rpc modules: {}", e))?;

            JsonRPCServer::new(
                mpc_rpc_api::server::Config {
                    host_address: local_party.rpc_addr,
                },
                module,
            )
            .await
            .map_err(|e| anyhow!("json rpc server terminated with err: {}", e))?
        };

        rpc_server.run().await.expect("expected RPC server to run");

        rt_task.abort();
        net_task.abort();
        let _ = rt_task.await;
        let _ = net_task.await;

        info!("Node stopped");

        Ok(())
    }
}
