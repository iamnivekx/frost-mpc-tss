pub mod error;
use crate::RpcResult;
use jsonrpsee::proc_macros::rpc;
use mpc_tss::{PublicKey, Signature};

#[rpc(server, client)]
pub trait TssApi {
    #[method(name = "keygen")]
    async fn keygen(&self, room: String, n: u16, t: u16) -> RpcResult<PublicKey>;

    #[method(name = "sign")]
    async fn sign(&self, room: String, t: u16, msg: Vec<u8>) -> RpcResult<Signature>;
}
