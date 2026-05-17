use crate::{
    negotiation::{NegotiationChannel, StartMsg},
    network_proxy::ReceiverProxy,
    peerset::Peerset,
    ComputeAgentAsync, PeersetMsg,
};
use futures::channel::{mpsc, oneshot};
use libp2p::PeerId;
use mpc_network::{
    request_responses, request_responses::MessageType, request_responses::OutgoingResponse,
    request_responses::SessionId, NetworkService, RoomId,
};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

const MAX_EARLY_COMPUTATION_MESSAGES: usize = 1024;

pub(crate) struct Phase1Channel {
    id: RoomId,
    rx: Option<mpsc::Receiver<request_responses::IncomingRequest>>,
    request: oneshot::Receiver<LocalRpcMsg>,
    network_service: NetworkService,
    buffered: VecDeque<request_responses::IncomingRequest>,
}

impl Phase1Channel {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<request_responses::IncomingRequest>,
        network_service: NetworkService,
    ) -> (Self, oneshot::Sender<LocalRpcMsg>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                id: room_id,
                rx: Some(room_rx),
                request: rx,
                network_service,
                buffered: VecDeque::new(),
            },
            tx,
        )
    }
}

impl Future for Phase1Channel {
    type Output = Phase1Msg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Ok(Some(msg)) = self.rx.as_mut().unwrap().try_next() {
            match msg.context.message_type {
                MessageType::Coordination => {
                    println!(
                        "Phase1Channel: Received coordination message from peer {}, room: {:?}",
                        msg.peer_id, self.id
                    );
                    return Poll::Ready(Phase1Msg::FromRemote {
                        peer_id: msg.peer_id,
                        protocol_id: msg.context.protocol_id,
                        payload: msg.payload,
                        response_tx: msg.pending_response,
                        channel: Phase2Channel {
                            id: self.id,
                            session_id: msg.context.session_id,
                            rx: self.rx.take(),
                            buffered: self.buffered.split_off(0),
                            timeout: Box::pin(tokio::time::sleep(Duration::from_secs(15))),
                            network_service: self.network_service.clone(),
                        },
                    });
                }
                MessageType::Computation => {
                    buffer_early_computation(&mut self.buffered, msg);
                }
            }
        }

        if let Some(LocalRpcMsg {
            n,
            payload: request,
            agent,
            pending_response,
        }) = self.request.try_recv().unwrap()
        {
            reject_buffered(self.buffered.split_off(0));
            return Poll::Ready(Phase1Msg::FromLocal {
                id: self.id.clone(),
                n,
                negotiation: NegotiationChannel::new(
                    self.id,
                    self.rx.take().unwrap(),
                    n,
                    request,
                    self.network_service.clone(),
                    agent,
                    pending_response,
                ),
            });
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum Phase1Msg {
    FromRemote {
        peer_id: PeerId,
        protocol_id: u64,
        payload: Vec<u8>,                               // for negotiation and stuff
        response_tx: oneshot::Sender<OutgoingResponse>, // respond if negotiation is fine
        channel: Phase2Channel,                         // listens after we respond
    },
    FromLocal {
        id: RoomId,
        n: u16,
        negotiation: NegotiationChannel,
    },
}

pub(crate) struct Phase2Channel {
    id: RoomId,
    session_id: SessionId,
    rx: Option<mpsc::Receiver<request_responses::IncomingRequest>>,
    buffered: VecDeque<request_responses::IncomingRequest>,
    timeout: Pin<Box<dyn Future<Output = ()> + Send>>,
    network_service: NetworkService,
}

impl Phase2Channel {
    pub fn room_id(&self) -> RoomId {
        self.id
    }

    pub fn abort(mut self) -> (RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>) {
        let (ch, tx) = Phase1Channel::new(
            self.id.clone(),
            self.rx.take().unwrap(),
            self.network_service,
        );
        reject_buffered(self.buffered);
        return (self.id, ch, tx);
    }
}

impl Future for Phase2Channel {
    type Output = Phase2Msg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => match msg.context.message_type {
                MessageType::Coordination => {
                    if msg.context.session_id != self.session_id {
                        let _ = msg.pending_response.send(OutgoingResponse {
                            result: Err(()),
                            sent_feedback: None,
                        });
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }

                    let (start_msg, peerset_rx) = match StartMsg::from_bytes(
                        &*msg.payload,
                        self.network_service.local_peer_id(),
                    ) {
                        Ok(res) => res,
                        Err(_) => {
                            reject_buffered(self.buffered.split_off(0));
                            let (ch, tx) = Phase1Channel::new(
                                self.id.clone(),
                                self.rx.take().unwrap(),
                                self.network_service.clone(),
                            );
                            return Poll::Ready(Phase2Msg::Abort(self.id.clone(), ch, tx));
                        }
                    };
                    let peerset = start_msg.peerset; // todo: check with cache
                    let (proxy, rx) = ReceiverProxy::new(
                        self.id.clone(),
                        self.rx.take().unwrap(),
                        self.network_service.clone(),
                        peerset.clone(),
                        self.session_id,
                        self.buffered.split_off(0),
                    );
                    return Poll::Ready(Phase2Msg::Start {
                        room_id: self.id.clone(),
                        room_receiver: rx,
                        receiver_proxy: proxy,
                        peerset,
                        peerset_rx,
                        init_body: start_msg.body,
                        session_id: self.session_id,
                    });
                }
                MessageType::Computation => {
                    if msg.context.session_id == self.session_id {
                        buffer_early_computation(&mut self.buffered, msg);
                    } else {
                        let _ = msg.pending_response.send(OutgoingResponse {
                            result: Err(()),
                            sent_feedback: None,
                        });
                    }
                }
            },
            _ => {}
        }

        // Remote peer gone offline or refused taking in us in set - returning to Phase 1

        if let Poll::Ready(()) = Future::poll(self.timeout.as_mut(), cx) {
            reject_buffered(self.buffered.split_off(0));
            let (ch, tx) = Phase1Channel::new(
                self.id.clone(),
                self.rx.take().unwrap(),
                self.network_service.clone(),
            );
            return Poll::Ready(Phase2Msg::Abort(self.id.clone(), ch, tx));
        }

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum Phase2Msg {
    Start {
        room_id: RoomId,
        room_receiver: mpsc::Receiver<request_responses::IncomingRequest>,
        receiver_proxy: ReceiverProxy,
        peerset: Peerset,
        peerset_rx: mpsc::Receiver<PeersetMsg>,
        init_body: Vec<u8>,
        session_id: SessionId,
    },
    Abort(RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>),
}

pub(crate) struct LocalRpcMsg {
    pub n: u16,
    pub payload: Vec<u8>,
    pub agent: Box<dyn ComputeAgentAsync>,
    pub pending_response: oneshot::Sender<anyhow::Result<Vec<u8>>>,
}

fn buffer_early_computation(
    buffered: &mut VecDeque<request_responses::IncomingRequest>,
    msg: request_responses::IncomingRequest,
) {
    if buffered.len() >= MAX_EARLY_COMPUTATION_MESSAGES {
        let _ = msg.pending_response.send(OutgoingResponse {
            result: Err(()),
            sent_feedback: None,
        });
        return;
    }

    buffered.push_back(msg);
}

fn reject_buffered(buffered: VecDeque<request_responses::IncomingRequest>) {
    for msg in buffered {
        let _ = msg.pending_response.send(OutgoingResponse {
            result: Err(()),
            sent_feedback: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::oneshot;
    use libp2p::PeerId;
    use mpc_network::request_responses::{
        IncomingRequest, MessageContext, MessageType, NO_SESSION_ID,
    };

    fn incoming_computation() -> (IncomingRequest, oneshot::Receiver<OutgoingResponse>) {
        let (tx, rx) = oneshot::channel();
        let peer_id = PeerId::random();
        (
            IncomingRequest {
                context: MessageContext {
                    message_type: MessageType::Computation,
                    protocol_id: 1,
                    session_id: NO_SESSION_ID,
                    message_id: 0,
                },
                peer: peer_id,
                peer_id,
                is_broadcast: true,
                peer_index: 0,
                payload: vec![1, 2, 3],
                pending_response: tx,
            },
            rx,
        )
    }

    #[test]
    fn early_computation_is_buffered_until_execution_receiver_exists() {
        let (msg, mut response_rx) = incoming_computation();
        let mut buffered = VecDeque::new();

        buffer_early_computation(&mut buffered, msg);

        assert_eq!(buffered.len(), 1);
        assert!(response_rx.try_recv().unwrap().is_none());
    }

    #[test]
    fn overflowing_early_computation_is_rejected() {
        let mut buffered = VecDeque::new();
        for _ in 0..MAX_EARLY_COMPUTATION_MESSAGES {
            let (msg, _) = incoming_computation();
            buffer_early_computation(&mut buffered, msg);
        }

        let (msg, mut response_rx) = incoming_computation();
        buffer_early_computation(&mut buffered, msg);

        assert_eq!(buffered.len(), MAX_EARLY_COMPUTATION_MESSAGES);
        assert!(response_rx.try_recv().unwrap().is_some());
    }
}
