use async_trait::async_trait;
use futures::channel::oneshot;
use mpc_network::RoomId;
use mpc_rpc_api::{tss::error::Error as TssError, RpcResult, TssApiServer};
use mpc_tss::{PublicKey, Signature};
use std::io::{BufWriter, Write};
use tracing::debug;

pub struct Tss {
    rt_service: mpc_runtime::Service,
}

impl Tss {
    pub fn new(rt_service: mpc_runtime::Service) -> Self {
        Self { rt_service }
    }
}

#[async_trait]
impl TssApiServer for Tss {
    async fn keygen(&self, room: String, n: u16, t: u16) -> RpcResult<PublicKey> {
        let mut rt_service = self.rt_service.clone();

        let (tx, rx) = oneshot::channel();
        let mut io = BufWriter::new(vec![]);
        let mut buffer = unsigned_varint::encode::u16_buffer();
        let _ = io.write_all(unsigned_varint::encode::u16(t, &mut buffer));
        let room_id = RoomId::from(room);
        let payload = io.buffer().to_vec();

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
        let room_id = RoomId::from(room.clone());
        println!("RPC sign: room name = '{room}', room_id = {room_id:?}");
        println!("RPC sign: Make sure all nodes use the same room name! Expected: 'tss/0'");

        rt_service.keysign(t + 1, room_id, msg, tx).await;

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
