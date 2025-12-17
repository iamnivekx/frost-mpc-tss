use async_std::channel::Sender;
use async_std::task;
use futures::channel::oneshot;
use mpc_rpc_api::{system::error::Error as SystemError, RpcFuture, RpcResult, SystemApi};
use serde::de::DeserializeOwned;
use std::fmt::Debug;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct System {
    send_back: Sender<Request>,
}

pub enum Request {
    /// Must return the base58-encoded local `PeerId`.
    LocalPeerId(oneshot::Sender<String>),
    /// Must return information about the peers we are connected to.
    Peers(oneshot::Sender<Vec<String>>),
}

impl System {
    pub fn new(send_back: Sender<Request>) -> Self {
        Self { send_back }
    }
}

impl SystemApi for System {
    fn system_local_peer_id(&self) -> RpcFuture<RpcResult<String>> {
        let send_back = self.send_back.clone();
        let (tx, rx) = oneshot::channel();
        task::spawn(async move {
            send_back
                .send(Request::LocalPeerId(tx))
                .await
                .expect("Failed to request to the peer task");
        });

        AsyncResult::new_boxed(rx)
    }

    fn system_peers(&self) -> RpcFuture<RpcResult<Vec<String>>> {
        let send_back = self.send_back.clone();
        let (tx, rx) = oneshot::channel();
        task::spawn(async move {
            send_back
                .send(Request::LocalPeerId(tx))
                .await
                .expect("Failed to request to the peers task");
        });
        AsyncResult::new_boxed(rx)
    }
}

#[derive(Debug)]
struct AsyncResult<T> {
    rx: oneshot::Receiver<String>,
    _marker: PhantomData<T>,
}

impl<T: DeserializeOwned + Unpin> Future for AsyncResult<T> {
    type Output = RpcResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.try_recv() {
            Ok(Some(value)) => Poll::Ready(
                serde_json::from_str(&value).map_err(|e| SystemError::from(e.to_string()).into()),
            ),
            Ok(None) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(SystemError::from(e).into())),
        }
    }
}

impl<T> AsyncResult<T> {
    pub fn new_boxed(rx: oneshot::Receiver<String>) -> Pin<Box<Self>> {
        Box::pin(Self {
            rx,
            _marker: Default::default(),
        })
    }
}
