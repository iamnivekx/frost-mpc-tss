use crate::{
    echo::{EchoMessage, EchoResponse},
    peerset::Peerset,
    ComputeAgentAsync, LocalStorage, PeersetMsg, PeersetStorage,
};
use anyhow::anyhow;
use futures::{
    channel::{mpsc, oneshot},
    Stream,
};
use futures_util::{stream::FuturesOrdered, FutureExt, StreamExt};
use libp2p::PeerId;
use mpc_network::{
    request_responses, request_responses::MessageContext, request_responses::MessageType,
    request_responses::OutgoingResponse, request_responses::SessionId, NetworkService, RoomId,
};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tracing::{error, info, warn};

const COMPLETION_QUIET_PERIOD: Duration = Duration::from_millis(500);

pub(crate) struct ProtocolExecution {
    state: Option<ProtocolExecState>,
}

struct ProtocolExecState {
    room_id: RoomId,
    session_id: SessionId,
    local_peer_id: PeerId,
    protocol_id: u64,
    network_service: NetworkService,
    peerset: Peerset,
    peerset_rx: mpsc::Receiver<PeersetMsg>,
    from_network: mpsc::Receiver<request_responses::IncomingRequest>,
    to_protocol: async_channel::Sender<crate::IncomingRequest>,
    from_protocol: async_channel::Receiver<crate::OutgoingResponse>,
    echo_tx: mpsc::Sender<EchoMessage>,
    agent_future: Option<Pin<Box<dyn Future<Output = anyhow::Result<Vec<u8>>> + Send>>>,
    agent_result: Option<anyhow::Result<Vec<u8>>>,
    completion_delay: Option<Pin<Box<tokio::time::Sleep>>>,
    pending_futures: FuturesOrdered<Pin<Box<dyn Future<Output = ()> + Send>>>,
    storage: LocalStorage,
    pending_response: Option<oneshot::Sender<anyhow::Result<Vec<u8>>>>,
    next_outgoing_broadcast_id: u64,
    next_incoming_broadcast_id: u64,
    pending_incoming: BTreeMap<(u16, u64), request_responses::IncomingRequest>,
    i: u16,
    n: u16,
}

impl ProtocolExecution {
    pub fn new(
        room_id: RoomId,
        session_id: SessionId,
        request: Vec<u8>,
        agent: Box<dyn ComputeAgentAsync>,
        network_service: NetworkService,
        peerset: Peerset,
        peerset_rx: mpsc::Receiver<PeersetMsg>,
        storage: LocalStorage,
        from_network: mpsc::Receiver<request_responses::IncomingRequest>,
        echo_tx: mpsc::Sender<EchoMessage>,
        pending_response: Option<oneshot::Sender<anyhow::Result<Vec<u8>>>>,
    ) -> Self {
        let n = peerset.size() as u16;
        let i = peerset.index_of(peerset.local_peer_id()).unwrap();
        let protocol_id = agent.protocol_id();
        let (to_protocol, from_runtime) = async_channel::bounded((n - 1) as usize);
        let (to_runtime, from_protocol) = async_channel::bounded((n - 1) as usize);

        let agent_future = agent.compute(peerset.clone(), request, from_runtime, to_runtime);

        Self {
            state: Some(ProtocolExecState {
                room_id,
                session_id,
                local_peer_id: network_service.local_peer_id(),
                protocol_id,
                network_service,
                peerset,
                peerset_rx,
                from_network,
                to_protocol,
                from_protocol,
                echo_tx,
                agent_future: Some(agent_future),
                agent_result: None,
                completion_delay: None,
                pending_futures: FuturesOrdered::new(),
                storage,
                pending_response,
                next_outgoing_broadcast_id: 0,
                next_incoming_broadcast_id: 0,
                pending_incoming: BTreeMap::new(),
                i,
                n,
            }),
        }
    }
}

impl Future for ProtocolExecution {
    type Output = crate::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let ProtocolExecState {
            room_id,
            session_id,
            local_peer_id,
            protocol_id,
            network_service,
            peerset,
            peerset_rx: mut from_peerset,
            mut from_network,
            to_protocol,
            mut from_protocol,
            mut echo_tx,
            mut agent_future,
            mut agent_result,
            mut completion_delay,
            mut pending_futures,
            mut storage,
            pending_response,
            mut next_outgoing_broadcast_id,
            mut next_incoming_broadcast_id,
            mut pending_incoming,
            i,
            n,
        } = self.state.take().unwrap();

        if let Poll::Ready(Some(message)) = Stream::poll_next(Pin::new(&mut from_peerset), cx) {
            match message {
                PeersetMsg::ReadFromCache(tx) => {
                    let _ = tx.send(storage.read_peerset(&room_id));
                }
                PeersetMsg::WriteToCache(peerset, tx) => {
                    let _ = tx.send(storage.write_peerset(&room_id, peerset));
                }
            }
        }

        if let Poll::Ready(Some(message)) = Stream::poll_next(Pin::new(&mut from_protocol), cx) {
            info!(
                "outgoing message to {:?}, body size: {} bytes",
                message.to,
                message.body.len()
            );

            match message.to {
                Some(remote_index) => {
                    let (res_tx, mut res_rx) = mpsc::channel(1);

                    pending_futures.push_back(
                        network_service
                            .clone()
                            .send_message_owned(
                                room_id.clone(),
                                peerset[remote_index - 1],
                                MessageContext {
                                    message_type: MessageType::Computation,
                                    protocol_id,
                                    session_id,
                                    message_id: 0,
                                },
                                message.body,
                                res_tx,
                            )
                            .boxed(),
                    );

                    // todo: handle in same Future::poll
                    tokio::task::spawn(async move {
                        if let Err(e) = res_rx.select_next_some().await {
                            error!("party responded with error: {e}");
                        }
                    });

                    if let Some(tx) = message.sent_feedback {
                        let _ = tx.send(());
                    }
                }
                None => {
                    // Broadcast message during protocol execution should use Computation type
                    let message_id = next_outgoing_broadcast_id;
                    next_outgoing_broadcast_id += 1;
                    let (res_tx, res_rx) = mpsc::channel((n - 1) as usize);
                    pending_futures.push_back(
                        network_service
                            .clone()
                            .multicast_message_owned(
                                room_id.clone(),
                                peerset.clone().remotes_iter(),
                                MessageContext {
                                    message_type: MessageType::Computation,
                                    protocol_id,
                                    session_id,
                                    message_id,
                                },
                                message.body.clone(),
                                Some(res_tx),
                            )
                            .boxed(),
                    );

                    echo_tx
                        .try_send(EchoMessage {
                            message_id,
                            sender: i + 1,
                            payload: message.body,
                            response: EchoResponse::Outgoing(res_rx),
                        })
                        .expect("echo channel is expected to be open");
                }
            }
        }

        loop {
            if let Poll::Ready(None) =
                Stream::poll_next(Pin::new(&mut pending_futures).as_mut(), cx)
            {
                break;
            }
        }

        if let Poll::Ready(Some(message)) = Stream::poll_next(Pin::new(&mut from_network), cx) {
            info!("incoming message from {}", message.peer_id.to_base58());
            if message.context.session_id != session_id
                || !matches!(message.context.message_type, MessageType::Computation)
            {
                let _ = message.pending_response.send(OutgoingResponse {
                    result: Err(()),
                    sent_feedback: None,
                });
                let _ = self.state.insert(ProtocolExecState {
                    room_id,
                    session_id,
                    local_peer_id,
                    protocol_id,
                    network_service,
                    peerset,
                    peerset_rx: from_peerset,
                    from_network,
                    to_protocol,
                    from_protocol,
                    echo_tx,
                    agent_future,
                    agent_result,
                    completion_delay,
                    pending_futures,
                    storage,
                    pending_response,
                    next_outgoing_broadcast_id,
                    next_incoming_broadcast_id,
                    pending_incoming,
                    i,
                    n,
                });
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }

            if message.is_broadcast {
                let sender = message.peer_index + 1;
                let message_id = message.context.message_id;
                pending_incoming.insert((sender, message_id), message);
            } else {
                forward_direct_message(message, &to_protocol, i);
            }

            if agent_result.is_some() {
                completion_delay = Some(Box::pin(tokio::time::sleep(COMPLETION_QUIET_PERIOD)));
            }
        }

        forward_buffered_messages(
            &mut pending_incoming,
            &mut next_incoming_broadcast_id,
            &to_protocol,
            &mut echo_tx,
            n,
        );

        if let Some(delay) = completion_delay.as_mut() {
            if Future::poll(delay.as_mut(), cx).is_ready() && pending_futures.is_empty() {
                let result = agent_result
                    .take()
                    .expect("completion delay is only set after agent completion");
                return match result {
                    Ok(res) => {
                        if let Some(tx) = pending_response {
                            if let Err(e) = tx.send(Ok(res)) {
                                error!("Failed to send result to RPC: {:?}", e);
                            }
                        } else {
                            warn!("No pending_response channel to send result");
                        }
                        Poll::Ready(Ok(()))
                    }
                    Err(e) => {
                        error!("Protocol execution failed: {:?}", e);
                        let err = anyhow!("{e}");
                        if let Some(tx) = pending_response {
                            if let Err(send_err) = tx.send(Err(e)) {
                                error!("Failed to send error to RPC: {:?}", send_err);
                            }
                        }
                        Poll::Ready(Err(crate::Error::InternalError(err)))
                    }
                };
            }
        } else if let Some(future) = agent_future.as_mut() {
            match Future::poll(future.as_mut(), cx) {
                Poll::Ready(res) => {
                    agent_result = Some(res);
                    agent_future = None;
                    completion_delay = Some(Box::pin(tokio::time::sleep(COMPLETION_QUIET_PERIOD)));
                }
                Poll::Pending => {}
            }
        }

        let _ = self.state.insert(ProtocolExecState {
            room_id,
            session_id,
            local_peer_id,
            protocol_id,
            network_service,
            peerset,
            peerset_rx: from_peerset,
            from_network,
            to_protocol,
            from_protocol,
            echo_tx,
            agent_future,
            agent_result,
            completion_delay,
            pending_futures,
            storage,
            pending_response,
            next_outgoing_broadcast_id,
            next_incoming_broadcast_id,
            pending_incoming,
            i,
            n,
        });

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

fn forward_buffered_messages(
    pending_incoming: &mut BTreeMap<(u16, u64), request_responses::IncomingRequest>,
    next_incoming_broadcast_id: &mut u64,
    to_protocol: &async_channel::Sender<crate::IncomingRequest>,
    echo_tx: &mut mpsc::Sender<EchoMessage>,
    n: u16,
) {
    loop {
        let message_id = *next_incoming_broadcast_id;
        let ready_senders = pending_incoming
            .keys()
            .filter_map(|(sender, pending_message_id)| {
                (*pending_message_id == message_id).then_some(*sender)
            })
            .collect::<Vec<_>>();

        if ready_senders.len() != (n - 1) as usize {
            break;
        }

        for sender in ready_senders {
            let message = pending_incoming
                .remove(&(sender, message_id))
                .expect("ready key must exist");

            echo_tx
                .try_send(EchoMessage {
                    message_id,
                    sender,
                    payload: message.payload.clone(),
                    response: EchoResponse::Incoming(message.pending_response),
                })
                .expect("echo channel is expected to be open");

            to_protocol
                .try_send(crate::IncomingRequest {
                    from: sender,
                    to: None,
                    payload: message.payload,
                })
                .expect("application channel is expected to be open");
        }

        *next_incoming_broadcast_id += 1;
    }
}

fn forward_direct_message(
    message: request_responses::IncomingRequest,
    to_protocol: &async_channel::Sender<crate::IncomingRequest>,
    local_index: u16,
) {
    let sender = message.peer_index + 1;
    if message
        .pending_response
        .send(OutgoingResponse {
            result: Ok(vec![]),
            sent_feedback: None,
        })
        .is_err()
    {
        warn!("failed sending acknowledgement to remote");
    }

    to_protocol
        .try_send(crate::IncomingRequest {
            from: sender,
            to: Some(local_index + 1),
            payload: message.payload,
        })
        .expect("application channel is expected to be open");
}
