use crate::coordination::{LocalRpcMsg, Phase1Channel};
use crate::peerset::Peerset;
use futures::channel::{mpsc, oneshot};
use mpc_network::{request_responses, request_responses::IncomingRequest, NetworkService, RoomId};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct ReceiverProxy {
    room_id: RoomId,
    request_receiver: Option<mpsc::Receiver<IncomingRequest>>,
    tx: mpsc::Sender<request_responses::IncomingRequest>,
    network_service: NetworkService,
    peerset: Peerset,
}

impl ReceiverProxy {
    pub fn new(
        room_id: RoomId,
        request_receiver: mpsc::Receiver<IncomingRequest>,
        network_service: NetworkService,
        peerset: Peerset,
    ) -> (Self, mpsc::Receiver<IncomingRequest>) {
        let (tx, rx) = mpsc::channel(peerset.size() - 1);
        (
            Self {
                room_id,
                request_receiver: Some(request_receiver),
                tx,
                network_service,
                peerset,
            },
            rx,
        )
    }
}

impl Future for ReceiverProxy {
    type Output = (RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.tx.is_closed() {
            let (ch, tx) = Phase1Channel::new(
                self.room_id.clone(),
                self.request_receiver.take().unwrap(),
                self.network_service.clone(),
            );
            return Poll::Ready((self.room_id.clone(), ch, tx));
        }

        match self.request_receiver.as_mut().unwrap().try_next() {
            Ok(Some(mut msg)) => match self.peerset.index_of(&msg.peer_id) {
                Some(i) => {
                    println!("polling receiver proxy {:?} : ", self.peerset.peers());
                    msg.peer_index = i;
                    let _ = self.tx.try_send(msg);
                }
                None => {
                    panic!("received message from unknown peer");
                }
            },
            _ => {}
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
