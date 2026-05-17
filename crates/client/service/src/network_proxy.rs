use crate::coordination::{LocalRpcMsg, Phase1Channel};
use crate::peerset::Peerset;
use futures::channel::{mpsc, oneshot};
use mpc_network::{
    request_responses, request_responses::IncomingRequest, request_responses::MessageType,
    request_responses::OutgoingResponse, request_responses::SessionId, NetworkService, RoomId,
};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct ReceiverProxy {
    room_id: RoomId,
    request_receiver: Option<mpsc::Receiver<IncomingRequest>>,
    tx: mpsc::Sender<request_responses::IncomingRequest>,
    network_service: NetworkService,
    peerset: Peerset,
    session_id: SessionId,
    buffered: VecDeque<IncomingRequest>,
}

impl ReceiverProxy {
    pub fn new(
        room_id: RoomId,
        request_receiver: mpsc::Receiver<IncomingRequest>,
        network_service: NetworkService,
        peerset: Peerset,
        session_id: SessionId,
        buffered: VecDeque<IncomingRequest>,
    ) -> (Self, mpsc::Receiver<IncomingRequest>) {
        let (tx, rx) = mpsc::channel(1024);
        (
            Self {
                room_id,
                request_receiver: Some(request_receiver),
                tx,
                network_service,
                peerset,
                session_id,
                buffered,
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

        if let Some(msg) = self.buffered.pop_front() {
            let session_id = self.session_id;
            let peerset = self.peerset.clone();
            forward_or_reject(msg, session_id, &peerset, &mut self.tx);
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        if let Ok(Some(msg)) = self.request_receiver.as_mut().unwrap().try_next() {
            let session_id = self.session_id;
            let peerset = self.peerset.clone();
            forward_or_reject(msg, session_id, &peerset, &mut self.tx);
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

fn forward_or_reject(
    mut msg: IncomingRequest,
    session_id: SessionId,
    peerset: &Peerset,
    tx: &mut mpsc::Sender<IncomingRequest>,
) {
    if !should_forward_message(&msg, session_id) {
        let _ = msg.pending_response.send(OutgoingResponse {
            result: Err(()),
            sent_feedback: None,
        });
        return;
    }

    match peerset.index_of(&msg.peer_id) {
        Some(i) => {
            println!("polling receiver proxy {:?} : ", peerset.peers());
            msg.peer_index = i;
            if let Err(e) = tx.try_send(msg) {
                eprintln!("receiver proxy channel is full or closed: {e}");
            }
        }
        None => {
            let _ = msg.pending_response.send(OutgoingResponse {
                result: Err(()),
                sent_feedback: None,
            });
        }
    }
}

fn should_forward_message(msg: &IncomingRequest, session_id: SessionId) -> bool {
    matches!(msg.context.message_type, MessageType::Computation)
        && msg.context.session_id == session_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::oneshot;
    use libp2p::PeerId;
    use mpc_network::request_responses::{MessageContext, NO_SESSION_ID};

    fn incoming(session_id: SessionId, message_type: MessageType) -> IncomingRequest {
        let (tx, _rx) = oneshot::channel();
        let peer_id = PeerId::random();
        IncomingRequest {
            context: MessageContext {
                message_type,
                protocol_id: 1,
                session_id,
                message_id: 0,
            },
            peer: peer_id,
            peer_id,
            is_broadcast: false,
            peer_index: 0,
            payload: vec![],
            pending_response: tx,
        }
    }

    #[test]
    fn receiver_proxy_only_forwards_current_computation_session() {
        let session_id = [7; 16];

        assert!(should_forward_message(
            &incoming(session_id, MessageType::Computation),
            session_id
        ));
        assert!(!should_forward_message(
            &incoming(NO_SESSION_ID, MessageType::Computation),
            session_id
        ));
        assert!(!should_forward_message(
            &incoming(session_id, MessageType::Coordination),
            session_id
        ));
    }
}
