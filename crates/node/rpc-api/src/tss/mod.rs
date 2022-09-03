pub mod error;
use crate::RpcResult;
use jsonrpc_core::BoxFuture;
use jsonrpc_derive::rpc;
use mpc_tss::{PublicKey, Signature};

#[rpc]
pub trait TssApi {
    #[rpc(name = "keygen")]
    fn keygen(&self, room: String, n: u16, t: u16) -> BoxFuture<RpcResult<PublicKey>>;

    #[rpc(name = "sign")]
    fn sign(&self, room: String, t: u16, msg: Vec<u8>) -> BoxFuture<RpcResult<Signature>>;
}
