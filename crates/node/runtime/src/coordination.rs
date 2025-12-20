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
    NetworkService, RoomId,
};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

pub(crate) struct Phase1Channel {
    id: RoomId,
    rx: Option<mpsc::Receiver<request_responses::IncomingRequest>>,
    request: oneshot::Receiver<LocalRpcMsg>,
    network_service: NetworkService,
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
                            rx: self.rx.take(),
                            timeout: Box::pin(tokio::time::sleep(Duration::from_secs(15))),
                            network_service: self.network_service.clone(),
                        },
                    });
                }
                MessageType::Computation => {
                    // Computation messages should be handled by ReceiverProxy during protocol execution
                    // Ignore them here - they will be processed by the active ReceiverProxy
                    // Send a response to acknowledge receipt
                    let _ = msg.pending_response.send(OutgoingResponse {
                        result: Ok(vec![]),
                        sent_feedback: None,
                    });
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
    rx: Option<mpsc::Receiver<request_responses::IncomingRequest>>,
    timeout: Pin<Box<dyn Future<Output = ()> + Send>>,
    network_service: NetworkService,
}

impl Phase2Channel {
    pub fn abort(mut self) -> (RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>) {
        let (ch, tx) = Phase1Channel::new(
            self.id.clone(),
            self.rx.take().unwrap(),
            self.network_service,
        );
        return (self.id, ch, tx);
    }
}

impl Future for Phase2Channel {
    type Output = Phase2Msg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().unwrap().try_next() {
            Ok(Some(msg)) => match msg.context.message_type {
                MessageType::Coordination => {
                    let (start_msg, peerset_rx) = match StartMsg::from_bytes(
                        &*msg.payload,
                        self.network_service.local_peer_id(),
                    ) {
                        Ok(res) => res,
                        Err(_) => {
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
                    );
                    return Poll::Ready(Phase2Msg::Start {
                        room_id: self.id.clone(),
                        room_receiver: rx,
                        receiver_proxy: proxy,
                        peerset,
                        peerset_rx,
                        init_body: start_msg.body,
                    });
                }
                MessageType::Computation => {
                    // Computation messages should be handled by ReceiverProxy during protocol execution
                    // Ignore them here - they will be processed by the active ReceiverProxy
                    // Send a response to acknowledge receipt
                    let _ = msg.pending_response.send(OutgoingResponse {
                        result: Ok(vec![]),
                        sent_feedback: None,
                    });
                }
            },
            _ => {}
        }

        // Remote peer gone offline or refused taking in us in set - returning to Phase 1

        if let Poll::Ready(()) = Future::poll(self.timeout.as_mut(), cx) {
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
    },
    Abort(RoomId, Phase1Channel, oneshot::Sender<LocalRpcMsg>),
}

pub(crate) struct LocalRpcMsg {
    pub n: u16,
    pub payload: Vec<u8>,
    pub agent: Box<dyn ComputeAgentAsync>,
    pub pending_response: oneshot::Sender<anyhow::Result<Vec<u8>>>,
}
