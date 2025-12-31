use crate::coordination::LocalRpcMsg;
use crate::coordination::Phase2Msg;
use crate::echo::EchoGadget;
use crate::execution::ProtocolExecution;
use crate::negotiation::NegotiationMsg;
use crate::ServicetoWorkerMsg;
use crate::{coordination, LocalStorage, ProtocolAgentFactory};
use anyhow::anyhow;
use futures::{channel::mpsc, stream::Fuse, StreamExt};
use futures_util::{select, stream::FuturesUnordered};
use mpc_network::request_responses::OutgoingResponse;
use mpc_network::{request_responses, NetworkService, RoomId};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use tracing::error;

pub struct Worker<T> {
    /// Channel receiver for messages send by a [`crate::Service`].
    from_service: Fuse<mpsc::Receiver<ServicetoWorkerMsg>>,
    network_service: NetworkService,
    rooms: HashMap<RoomId, mpsc::Receiver<request_responses::IncomingRequest>>,
    client: T,
    peerset_storage: LocalStorage,
}

impl<T: ProtocolAgentFactory + Send + Unpin> Worker<T> {
    pub fn new(
        from_service: mpsc::Receiver<ServicetoWorkerMsg>,
        network_service: NetworkService,
        rooms: impl Iterator<Item = (RoomId, mpsc::Receiver<request_responses::IncomingRequest>)>,
        client: T,
        peerset_storage: LocalStorage,
    ) -> Self {
        Worker {
            from_service: from_service.fuse(),
            network_service,
            rooms: rooms.collect(),
            client,
            peerset_storage,
        }
    }

    pub async fn run(mut self) {
        let mut protocol_executions = FuturesUnordered::new();
        let mut network_proxies = FuturesUnordered::new();
        let mut rooms_coordination = FuturesUnordered::new();
        let mut rooms_rpc = HashMap::new();

        let Self {
            network_service,
            rooms,
            client,
            peerset_storage,
            ..
        } = self;

        for (room_id, rx) in rooms.into_iter() {
            let (ch, tx) =
                coordination::Phase1Channel::new(room_id.clone(), rx, network_service.clone());
            rooms_coordination.push(ch);
            rooms_rpc.insert(room_id, tx);
        }

        loop {
            select! {
                msg = self.from_service.select_next_some() => {
                    match msg {
                        ServicetoWorkerMsg::KeyGen(_t, n, room_id, payload, sender) => {
                            match rooms_rpc.entry(room_id.clone()) {
                                Entry::Occupied(entry) => {
                                    let agent = client.keygen();
                                    let e = entry.remove();

                                    if e.is_canceled() {
                                        let _ = sender.send(Err(anyhow!("sender is canceled")));
                                    } else {
                                        let _ = e.send(LocalRpcMsg{n, payload, agent, pending_response: sender});
                                    }
                                }
                                Entry::Vacant(_) => {
                                    // Dynamically create room if it doesn't exist
                                    // Create a virtual receiver for the room (network layer may not know about it yet)
                                    // Note: We keep virtual_tx to prevent the receiver from closing
                                    let (_virtual_tx, virtual_rx) = mpsc::channel(1000);
                                    let (ch, tx) = coordination::Phase1Channel::new(
                                        room_id.clone(),
                                        virtual_rx,
                                        network_service.clone(),
                                    );
                                    rooms_coordination.push(ch);

                                    let agent = client.keygen();
                                    if tx.is_canceled() {
                                        let _ = sender.send(Err(anyhow!("sender is canceled")));
                                    } else {
                                        match tx.send(LocalRpcMsg{n, payload, agent, pending_response: sender}) {
                                            Ok(_) => {}
                                            Err(_) => {
                                                // If send fails, the sender is still in LocalRpcMsg
                                                // This shouldn't happen, but if it does, we've lost the sender
                                                error!("Failed to send LocalRpcMsg to newly created room channel");
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        ServicetoWorkerMsg::KeySign(
                            n,
                            room_id,
                            payload,
                            sender,
                         ) => {
                            match rooms_rpc.entry(room_id.clone()) {
                                Entry::Occupied(entry) => {
                                    let agent =  client.keysign();
                                    let e = entry.remove();
                                    if e.is_canceled() {
                                        let _ = sender.send(Err(anyhow!("sender is canceled")));
                                    } else {
                                        let _ = e.send(LocalRpcMsg{n, payload, agent, pending_response: sender});
                                    }
                                }
                                Entry::Vacant(_) => {
                                    // Dynamically create room if it doesn't exist
                                    // Create a virtual receiver for the room (network layer may not know about it yet)
                                    // Note: We keep virtual_tx to prevent the receiver from closing
                                    let (_virtual_tx, virtual_rx) = mpsc::channel(1000);
                                    let (ch, tx) = coordination::Phase1Channel::new(
                                        room_id.clone(),
                                        virtual_rx,
                                        network_service.clone(),
                                    );
                                    rooms_coordination.push(ch);

                                    let agent = client.keysign();
                                    if tx.is_canceled() {
                                        let _ = sender.send(Err(anyhow!("sender is canceled")));
                                    } else {
                                        match tx.send(LocalRpcMsg{n, payload, agent, pending_response: sender}) {
                                            Ok(_) => {}
                                            Err(_) => {
                                                // If send fails, the sender is still in LocalRpcMsg
                                                // This shouldn't happen, but if it does, we've lost the sender
                                                error!("Failed to send LocalRpcMsg to newly created room channel");
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    }
                },
                coord_msg = rooms_coordination.select_next_some() => match coord_msg {
                coordination::Phase1Msg::FromRemote {
                    peer_id,
                    protocol_id,
                    payload: _,
                    response_tx,
                    channel,
                } => {
                    println!("Worker: Received coordination message from peer {}, protocol_id: {}", peer_id, protocol_id);
                    let agent = match client.make(protocol_id) {
                        Ok(a) => a,
                        Err(_) => {
                            println!("agent factory error");
                            let (id, ch, tx) = channel.abort();
                            rooms_coordination.push(ch);
                            rooms_rpc.insert(id, tx);
                            continue;
                        }
                    };

                    println!("Worker: Sending response to peer {}", peer_id);
                    let _ = response_tx.send(OutgoingResponse {
                        result: Ok(vec![]), // todo: real negotiation logic
                        sent_feedback: None,
                    });

                        match channel.await {
                            Phase2Msg::Start {
                                room_id,
                                room_receiver,
                                receiver_proxy,
                                peerset,
                                peerset_rx,
                                init_body,
                            } => {
                                network_proxies.push(receiver_proxy);
                                let (echo, echo_tx) = EchoGadget::new(peerset.size());
                                protocol_executions.push(echo.wrap_execution(ProtocolExecution::new(
                                    room_id,
                                    init_body,
                                    agent,
                                    network_service.clone(),
                                    peerset,
                                    peerset_rx,
                                    peerset_storage.clone(),
                                    room_receiver,
                                    echo_tx,
                                    None,
                                )));
                            }
                            Phase2Msg::Abort(room_id, ch, tx) => {
                                rooms_rpc.entry(room_id).and_modify(|e| *e = tx);
                                rooms_coordination.push(ch);
                            }
                        }
                    }
                    coordination::Phase1Msg::FromLocal {
                        id,
                        n,
                        negotiation,
                    } => {
                        match negotiation.await {
                            NegotiationMsg::Start {
                                agent,
                                pending_response,
                                room_receiver,
                                receiver_proxy,
                                peerset,
                                peerset_rx,
                                request,
                            } => {
                                network_proxies.push(receiver_proxy);
                                let (echo, echo_tx) = EchoGadget::new(n as usize);
                                protocol_executions.push(echo.wrap_execution(ProtocolExecution::new(
                                    id,
                                    request,
                                    agent,
                                    network_service.clone(),
                                    peerset,
                                    peerset_rx,
                                    peerset_storage.clone(),
                                    room_receiver,
                                    echo_tx,
                                    Some(pending_response),
                                )));
                            }
                            NegotiationMsg::Abort { room_id, phase1, rpc_tx, pending_response } => {
                                // Send error response if negotiation was aborted
                                if let Some(sender) = pending_response {
                                    let _ = sender.send(Err(anyhow!("Negotiation aborted: timeout or failed to assemble peerset")));
                                }
                                rooms_coordination.push(phase1);
                                rooms_rpc.insert(room_id, rpc_tx);
                                continue;
                            }
                        };
                    }
                },
                exec_res = protocol_executions.select_next_some() => match exec_res {
                    Ok(_) => {}
                    Err(e) => {
                        error!("error during computation: {e}");
                        // When protocol execution fails, we need to ensure the room is restored.
                        // The network_proxy should complete and restore the room, but if it doesn't,
                        // we'll need to handle it separately. For now, we rely on network_proxy
                        // completing when the execution fails.
                    }
                },
                (room_id, phase1, rpc_tx) = network_proxies.select_next_some() => {
                    rooms_coordination.push(phase1);
                    rooms_rpc.insert(room_id, rpc_tx);
                }
            }
        }
    }
}
