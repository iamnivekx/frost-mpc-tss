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
    request_responses::OutgoingResponse, NetworkService, RoomId,
};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tracing::{error, info, warn};

pub(crate) struct ProtocolExecution {
    state: Option<ProtocolExecState>,
}

struct ProtocolExecState {
    room_id: RoomId,
    local_peer_id: PeerId,
    protocol_id: u64,
    network_service: NetworkService,
    peerset: Peerset,
    peerset_rx: mpsc::Receiver<PeersetMsg>,
    from_network: mpsc::Receiver<request_responses::IncomingRequest>,
    to_protocol: async_channel::Sender<crate::IncomingRequest>,
    from_protocol: async_channel::Receiver<crate::OutgoingResponse>,
    echo_tx: mpsc::Sender<EchoMessage>,
    agent_future: Pin<Box<dyn Future<Output = anyhow::Result<Vec<u8>>> + Send>>,
    pending_futures: FuturesOrdered<Pin<Box<dyn Future<Output = ()> + Send>>>,
    storage: LocalStorage,
    pending_response: Option<oneshot::Sender<anyhow::Result<Vec<u8>>>>,
    i: u16,
    n: u16,
}

impl ProtocolExecution {
    pub fn new(
        room_id: RoomId,
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
                local_peer_id: network_service.local_peer_id(),
                protocol_id,
                network_service,
                peerset,
                peerset_rx,
                from_network,
                to_protocol,
                from_protocol,
                echo_tx,
                agent_future,
                pending_futures: FuturesOrdered::new(),
                storage,
                pending_response,
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
            mut pending_futures,
            mut storage,
            pending_response,
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
                                },
                                message.body.clone(),
                                Some(res_tx),
                            )
                            .boxed(),
                    );

                    echo_tx
                        .try_send(EchoMessage {
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

            if message.is_broadcast {
                echo_tx
                    .try_send(EchoMessage {
                        sender: message.peer_index + 1,
                        payload: message.payload.clone(),
                        response: EchoResponse::Incoming(message.pending_response),
                    })
                    .expect("echo channel is expected to be open");
            } else {
                if let Err(_) = message.pending_response.send(OutgoingResponse {
                    result: Ok(vec![]),
                    sent_feedback: None,
                }) {
                    warn!("failed sending acknowledgement to remote");
                }
            }

            to_protocol
                .try_send(crate::IncomingRequest {
                    from: message.peer_index + 1,
                    to: if message.is_broadcast {
                        None
                    } else {
                        Some(i + 1)
                    },
                    payload: message.payload,
                })
                .expect("application channel is expected to be open");
        }

        match Future::poll(Pin::new(&mut agent_future), cx) {
            Poll::Ready(Ok(res)) => {
                if let Some(tx) = pending_response {
                    if let Err(e) = tx.send(Ok(res)) {
                        error!("Failed to send result to RPC: {:?}", e);
                    }
                } else {
                    warn!("No pending_response channel to send result");
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => {
                error!("Protocol execution failed: {:?}", e);
                let err = anyhow!("{e}");
                if let Some(tx) = pending_response {
                    if let Err(send_err) = tx.send(Err(e)) {
                        error!("Failed to send error to RPC: {:?}", send_err);
                    }
                }
                Poll::Ready(Err(crate::Error::InternalError(err)))
            }
            Poll::Pending => {
                let _ = self.state.insert(ProtocolExecState {
                    room_id,
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
                    pending_futures,
                    storage,
                    pending_response,
                    i,
                    n,
                });

                // Wake this task to be polled again.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}
