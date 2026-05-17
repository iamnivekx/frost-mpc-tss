use crate::coordination::LocalRpcMsg;
use crate::coordination::Phase2Msg;
use crate::echo::EchoGadget;
use crate::execution::ProtocolExecution;
use crate::negotiation::NegotiationMsg;
use crate::ServicetoWorkerMsg;
use crate::{coordination, LocalStorage, ProtocolAgentFactory};
use anyhow::anyhow;
use futures::{channel::mpsc, channel::oneshot, future::BoxFuture, stream::Fuse, StreamExt};
use futures_util::{select, stream::FuturesUnordered};
use mpc_network::request_responses::OutgoingResponse;
use mpc_network::{request_responses, NetworkService, RoomId};
use std::collections::{HashMap, HashSet, VecDeque};
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
        let mut busy_rooms = HashSet::new();
        let mut pending_room_restores = HashMap::new();
        let mut pending_local = HashMap::new();

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
                    let room_id = service_msg_room_id(&msg);
                    if busy_rooms.contains(&room_id) {
                        pending_local
                            .entry(room_id)
                            .or_insert_with(VecDeque::new)
                            .push_back(msg);
                    } else if dispatch_local_msg(
                        msg,
                        &mut rooms_rpc,
                        &mut rooms_coordination,
                        &network_service,
                        &client,
                    ) {
                        busy_rooms.insert(room_id);
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
                    busy_rooms.insert(channel.room_id());
                    let agent = match client.make(protocol_id) {
                        Ok(a) => a,
                        Err(_) => {
                            println!("agent factory error");
                            let (id, ch, tx) = channel.abort();
                            rooms_coordination.push(ch);
                            rooms_rpc.insert(id, tx);
                            busy_rooms.remove(&id);
                            dispatch_pending_local(
                                id,
                                &mut pending_local,
                                &mut busy_rooms,
                                &mut rooms_rpc,
                                &mut rooms_coordination,
                                &network_service,
                                &client,
                            );
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
                                session_id,
                            } => {
                                busy_rooms.insert(room_id);
                                network_proxies.push(receiver_proxy);
                                let (echo, echo_tx) = EchoGadget::new(peerset.size());
                                let execution_room_id = room_id;
                                protocol_executions.push(wrap_protocol_execution(
                                    execution_room_id,
                                    echo.wrap_execution(ProtocolExecution::new(
                                    execution_room_id,
                                    session_id,
                                    init_body,
                                    agent,
                                    network_service.clone(),
                                    peerset,
                                    peerset_rx,
                                    peerset_storage.clone(),
                                    room_receiver,
                                    echo_tx,
                                    None,
                                ))));
                            }
                            Phase2Msg::Abort(room_id, ch, tx) => {
                                busy_rooms.remove(&room_id);
                                rooms_coordination.push(ch);
                                rooms_rpc.insert(room_id, tx);
                                dispatch_pending_local(
                                    room_id,
                                    &mut pending_local,
                                    &mut busy_rooms,
                                    &mut rooms_rpc,
                                    &mut rooms_coordination,
                                    &network_service,
                                    &client,
                                );
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
                                session_id,
                            } => {
                                busy_rooms.insert(id);
                                network_proxies.push(receiver_proxy);
                                let (echo, echo_tx) = EchoGadget::new(n as usize);
                                let execution_room_id = id;
                                protocol_executions.push(wrap_protocol_execution(
                                    execution_room_id,
                                    echo.wrap_execution(ProtocolExecution::new(
                                    execution_room_id,
                                    session_id,
                                    request,
                                    agent,
                                    network_service.clone(),
                                    peerset,
                                    peerset_rx,
                                    peerset_storage.clone(),
                                    room_receiver,
                                    echo_tx,
                                    Some(pending_response),
                                ))));
                            }
                            NegotiationMsg::Abort { room_id, phase1, rpc_tx, pending_response } => {
                                // Send error response if negotiation was aborted
                                if let Some(sender) = pending_response {
                                    let _ = sender.send(Err(anyhow!("Negotiation aborted: timeout or failed to assemble peerset")));
                                }
                                busy_rooms.remove(&room_id);
                                rooms_coordination.push(phase1);
                                rooms_rpc.insert(room_id, rpc_tx);
                                dispatch_pending_local(
                                    room_id,
                                    &mut pending_local,
                                    &mut busy_rooms,
                                    &mut rooms_rpc,
                                    &mut rooms_coordination,
                                    &network_service,
                                    &client,
                                );
                                continue;
                            }
                        };
                    }
                },
                (room_id, exec_res) = protocol_executions.select_next_some() => match exec_res {
                    Ok(_) => {
                        busy_rooms.remove(&room_id);
                        if let Some((phase1, rpc_tx)) = pending_room_restores.remove(&room_id) {
                            rooms_coordination.push(phase1);
                            rooms_rpc.insert(room_id, rpc_tx);
                            dispatch_pending_local(
                                room_id,
                                &mut pending_local,
                                &mut busy_rooms,
                                &mut rooms_rpc,
                                &mut rooms_coordination,
                                &network_service,
                                &client,
                            );
                        }
                    }
                    Err(e) => {
                        error!("error during computation: {e}");
                        busy_rooms.remove(&room_id);
                        if let Some((phase1, rpc_tx)) = pending_room_restores.remove(&room_id) {
                            rooms_coordination.push(phase1);
                            rooms_rpc.insert(room_id, rpc_tx);
                            dispatch_pending_local(
                                room_id,
                                &mut pending_local,
                                &mut busy_rooms,
                                &mut rooms_rpc,
                                &mut rooms_coordination,
                                &network_service,
                                &client,
                            );
                        }
                    }
                },
                (room_id, phase1, rpc_tx) = network_proxies.select_next_some() => {
                    if busy_rooms.contains(&room_id) {
                        pending_room_restores.insert(room_id, (phase1, rpc_tx));
                    } else {
                        rooms_coordination.push(phase1);
                        rooms_rpc.insert(room_id, rpc_tx);
                        dispatch_pending_local(
                            room_id,
                            &mut pending_local,
                            &mut busy_rooms,
                            &mut rooms_rpc,
                            &mut rooms_coordination,
                            &network_service,
                            &client,
                        );
                    }
                }
            }
        }
    }
}

type ProtocolExecutionResult = BoxFuture<'static, (RoomId, crate::Result<()>)>;

fn wrap_protocol_execution(
    room_id: RoomId,
    fut: impl std::future::Future<Output = crate::Result<()>> + Send + 'static,
) -> ProtocolExecutionResult {
    Box::pin(async move { (room_id, fut.await) })
}

fn service_msg_room_id(msg: &ServicetoWorkerMsg) -> RoomId {
    match msg {
        ServicetoWorkerMsg::KeySign(_, room_id, _, _) => *room_id,
        ServicetoWorkerMsg::KeyGen(_, _, room_id, _, _) => *room_id,
    }
}

fn dispatch_pending_local<T: ProtocolAgentFactory>(
    room_id: RoomId,
    pending_local: &mut HashMap<RoomId, VecDeque<ServicetoWorkerMsg>>,
    busy_rooms: &mut HashSet<RoomId>,
    rooms_rpc: &mut HashMap<RoomId, oneshot::Sender<LocalRpcMsg>>,
    rooms_coordination: &mut FuturesUnordered<coordination::Phase1Channel>,
    network_service: &NetworkService,
    client: &T,
) {
    if busy_rooms.contains(&room_id) {
        return;
    }

    let Some(queue) = pending_local.get_mut(&room_id) else {
        return;
    };
    let Some(msg) = queue.pop_front() else {
        pending_local.remove(&room_id);
        return;
    };
    if queue.is_empty() {
        pending_local.remove(&room_id);
    }

    if dispatch_local_msg(msg, rooms_rpc, rooms_coordination, network_service, client) {
        busy_rooms.insert(room_id);
    }
}

fn dispatch_local_msg<T: ProtocolAgentFactory>(
    msg: ServicetoWorkerMsg,
    rooms_rpc: &mut HashMap<RoomId, oneshot::Sender<LocalRpcMsg>>,
    rooms_coordination: &mut FuturesUnordered<coordination::Phase1Channel>,
    network_service: &NetworkService,
    client: &T,
) -> bool {
    match msg {
        ServicetoWorkerMsg::KeyGen(_t, n, room_id, payload, sender) => {
            let agent = client.keygen();
            dispatch_to_room(
                room_id,
                LocalRpcMsg {
                    n,
                    payload,
                    agent,
                    pending_response: sender,
                },
                rooms_rpc,
                rooms_coordination,
                network_service,
            )
        }
        ServicetoWorkerMsg::KeySign(n, room_id, payload, sender) => {
            let agent = client.keysign();
            dispatch_to_room(
                room_id,
                LocalRpcMsg {
                    n,
                    payload,
                    agent,
                    pending_response: sender,
                },
                rooms_rpc,
                rooms_coordination,
                network_service,
            )
        }
    }
}

fn dispatch_to_room(
    room_id: RoomId,
    local_msg: LocalRpcMsg,
    rooms_rpc: &mut HashMap<RoomId, oneshot::Sender<LocalRpcMsg>>,
    rooms_coordination: &mut FuturesUnordered<coordination::Phase1Channel>,
    network_service: &NetworkService,
) -> bool {
    let tx = match rooms_rpc.remove(&room_id) {
        Some(tx) => tx,
        None => {
            let (_virtual_tx, virtual_rx) = mpsc::channel(1000);
            let (ch, tx) =
                coordination::Phase1Channel::new(room_id, virtual_rx, network_service.clone());
            rooms_coordination.push(ch);
            tx
        }
    };

    if tx.is_canceled() {
        let _ = local_msg
            .pending_response
            .send(Err(anyhow!("sender is canceled")));
        false
    } else if tx.send(local_msg).is_err() {
        error!("Failed to send LocalRpcMsg to room channel");
        false
    } else {
        true
    }
}
