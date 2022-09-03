use async_std::task;
use futures::channel::oneshot;
use mpc_network::RoomId;
use mpc_rpc_api::{tss::error::Error as TssError, tss::TssApi, RpcFuture, RpcResult};
use mpc_tss::{PublicKey, Signature};
use serde::de::DeserializeOwned;
use std::fmt::{Debug, Display};
use std::future::Future;
use std::io::{BufWriter, Write};
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use tracing::debug;

pub struct Tss {
    rt_service: mpc_runtime::Service,
}

impl Tss {
    pub fn new(rt_service: mpc_runtime::Service) -> Self {
        Self { rt_service }
    }
}

impl TssApi for Tss {
    fn keygen(&self, room: String, n: u16, t: u16) -> RpcFuture<RpcResult<PublicKey>> {
        let mut rt_service = self.rt_service.clone();

        let (tx, rx) = oneshot::channel();
        let mut io = BufWriter::new(vec![]);
        let mut buffer = unsigned_varint::encode::u16_buffer();
        let _ = io.write_all(unsigned_varint::encode::u16(t, &mut buffer));
        let room_id = RoomId::from(room);
        // let payload = vec![];
        let payload = io.buffer().to_vec();

        task::spawn(async move {
            rt_service.keygen(t, n, payload, room_id, tx).await;
        });

        AsyncResult::new_boxed(rx)
    }

    fn sign(&self, room: String, t: u16, msg: Vec<u8>) -> RpcFuture<RpcResult<Signature>> {
        let mut rt_service = self.rt_service.clone();

        let (tx, rx) = oneshot::channel();
        let room_id = RoomId::from(room.clone());
        println!("RPC sign: room name = '{}', room_id = {:?}", room, room_id);
        println!("RPC sign: Make sure all nodes use the same room name! Expected: 'tss/0'");
        task::spawn(async move {
            rt_service.keysign(t + 1, room_id, msg, tx).await;
        });

        AsyncResult::new_boxed(rx)
    }
}

#[derive(Debug)]
struct AsyncResult<T, E> {
    rx: oneshot::Receiver<Result<Vec<u8>, E>>,
    _marker: PhantomData<T>,
}

impl<T: DeserializeOwned + Unpin, E: Display> Future for AsyncResult<T, E> {
    type Output = RpcResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        return match self.rx.try_recv() {
            Ok(Some(Ok(value))) => match serde_ipld_dagcbor::from_slice(&*value) {
                Ok(result) => Poll::Ready(Ok(result)),
                Err(e) => {
                    debug!("RPC: Failed to deserialize result: {}", e);
                    Poll::Ready(Err(TssError::from(e.to_string()).into()))
                }
            },
            Ok(Some(Err(e))) => {
                debug!("RPC: Received error: {}", e);
                Poll::Ready(Err(TssError::from(e.to_string()).into()))
            }
            Ok(None) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(e) => {
                println!("RPC: Channel error: {}", e);
                Poll::Ready(Err(TssError::from(e).into()))
            }
        };
    }
}

impl<T, E> AsyncResult<T, E> {
    pub fn new_boxed(rx: oneshot::Receiver<Result<Vec<u8>, E>>) -> Pin<Box<Self>> {
        Box::pin(Self {
            rx,
            _marker: Default::default(),
        })
    }
}
