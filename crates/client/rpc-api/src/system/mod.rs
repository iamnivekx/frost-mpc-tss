pub mod error;
use crate::RpcResult;
use jsonrpsee::proc_macros::rpc;

#[rpc(server, client, namespace = "system")]
pub trait SystemApi {
    /// Returns the base58-encoded PeerId of the node.
    #[method(name = "localPeerId")]
    async fn local_peer_id(&self) -> RpcResult<String>;

    /// Returns currently connected peers
    #[method(name = "peers")]
    async fn peers(&self) -> RpcResult<Vec<String>>;

    /// Returns the number of currently connected peers
    #[method(name = "numConnectedPeers")]
    async fn num_connected_peers(&self) -> RpcResult<usize>;
}
