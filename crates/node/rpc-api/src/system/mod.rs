pub mod error;
use jsonrpc_core::BoxFuture;
use jsonrpc_derive::rpc;

#[rpc]
pub trait SystemApi {
    /// Returns the base58-encoded PeerId of the node.
    #[rpc(name = "system_localPeerId", returns = "String")]
    fn system_local_peer_id(&self) -> BoxFuture<jsonrpc_core::Result<String>>;

    /// Returns currently connected peers
    #[rpc(name = "system_peers", returns = "Vec<String>")]
    fn system_peers(&self) -> BoxFuture<jsonrpc_core::Result<Vec<String>>>;
}
