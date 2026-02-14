use async_trait::async_trait;
use futures::channel::oneshot;
use mpc_network::RoomId;
use mpc_rpc_api::{tss::error::Error as TssError, RpcResult, TssApiServer};
use mpc_tss::{
    encode_keygen_payload, encode_sign_payload, PublicKey, Signature, DEFAULT_TRANSPORT_ROOM,
};
use tracing::debug;

pub struct Tss {
    rt_service: mpc_service::Service,
}

impl Tss {
    pub fn new(rt_service: mpc_service::Service) -> Self {
        Self { rt_service }
    }
}

#[async_trait]
impl TssApiServer for Tss {
    async fn keygen(&self, room: String, n: u16, t: u16) -> RpcResult<PublicKey> {
        let mut rt_service = self.rt_service.clone();

        let (tx, rx) = oneshot::channel();
        let wallet_id = room;
        let room_id = RoomId::from(DEFAULT_TRANSPORT_ROOM.to_owned());
        let payload = encode_keygen_payload(&wallet_id, t)
            .map_err(|e| TssError::from(format!("failed to encode keygen request: {e}")))?;

        rt_service.keygen(t, n, payload, room_id, tx).await;

        match rx.await {
            Ok(Ok(value)) => serde_ipld_dagcbor::from_slice(&value).map_err(|e| {
                debug!("RPC: Failed to deserialize result: {}", e);
                TssError::from(e.to_string()).into()
            }),
            Ok(Err(e)) => {
                debug!("RPC: Received error: {}", e);
                Err(TssError::from(e.to_string()).into())
            }
            Err(e) => Err(TssError::from(e).into()),
        }
    }

    async fn sign(&self, room: String, t: u16, msg: Vec<u8>) -> RpcResult<Signature> {
        let mut rt_service = self.rt_service.clone();

        let (tx, rx) = oneshot::channel();
        let wallet_id = room;
        let room_id = RoomId::from(DEFAULT_TRANSPORT_ROOM.to_owned());
        let payload = encode_sign_payload(&wallet_id, msg)
            .map_err(|e| TssError::from(format!("failed to encode sign request: {e}")))?;

        rt_service.keysign(t + 1, room_id, payload, tx).await;

        match rx.await {
            Ok(Ok(value)) => serde_ipld_dagcbor::from_slice(&value).map_err(|e| {
                debug!("RPC: Failed to deserialize result: {}", e);
                TssError::from(e.to_string()).into()
            }),
            Ok(Err(e)) => {
                debug!("RPC: Received error: {}", e);
                Err(TssError::from(e.to_string()).into())
            }
            Err(e) => Err(TssError::from(e).into()),
        }
    }
}
