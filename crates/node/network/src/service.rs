use crate::{
    behaviour::{Behaviour, BehaviourEvent},
    error::Error,
    request_responses,
    request_responses::IfDisconnected,
    request_responses::MessageContext,
    request_responses::RequestFailure,
    NodeKeyConfig, RoomId, SessionError, SessionManager,
};
use async_std::channel::{unbounded, Receiver, Sender};
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::select;
use futures_util::stream::StreamExt;
use libp2p::core::transport::upgrade;
use libp2p::noise::{Config as NoiseConfig, Keypair as NoiseKeypair, X25519Spec};
use libp2p::swarm::{SwarmBuilder, SwarmEvent};
use libp2p::tcp::TcpConfig;
use libp2p::{mplex, PeerId, Swarm, Transport};
use std::{borrow::Cow, sync::Arc};
use tracing::{debug, info, warn};

/// Events emitted by this Service.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum NetworkEvent {
    BroadcastMessage(PeerId, Cow<'static, str>),
}

/// Messages into the service to handle.
#[derive(Debug)]
pub enum ServicetoWorkerMsg {
    Request {
        room_id: RoomId,
        context: MessageContext,
        message: MessageIntent,
    },
    OpenRoom {
        room_id: RoomId,
        max_size: usize,
        respond_to: oneshot::Sender<
            Result<mpsc::Receiver<request_responses::IncomingRequest>, SessionError>,
        >,
    },
}

#[derive(Debug)]
pub enum MessageIntent {
    Broadcast(
        Vec<u8>,
        Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
    ),
    Multicast(
        Vec<PeerId>,
        Vec<u8>,
        Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
    ),
    SendDirect(
        PeerId,
        Vec<u8>,
        mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>,
    ),
}

/// Main network worker. Must be polled in order for the network to advance.
///
/// You are encouraged to poll this in a separate background thread or task.
#[must_use = "The NetworkWorker must be polled in order for the network to advance"]
pub struct NetworkWorker {
    pub local_peer_id: PeerId,
    /// The network service that can be extracted and shared through the codebase.
    pub service: Arc<NetworkService>,
    /// The *actual* network.
    network_service: Swarm<Behaviour>,
    /// Messages from the [`NetworkService`] that must be processed.
    from_service: Receiver<ServicetoWorkerMsg>,
    sessions: SessionManager,
    // / The `PeerId`'s of all boot nodes.
    // boot_node_ids: Arc<HashSet<PeerId>>,
}

#[derive(Clone)]
pub struct NetworkService {
    /// Local copy of the `PeerId` of the local node.
    local_peer_id: PeerId,
    /// Channel that sends messages to the actual worker.
    to_worker: Sender<ServicetoWorkerMsg>,
}

impl NetworkWorker {
    pub fn new(node_key: NodeKeyConfig, params: crate::Params) -> Result<Self, Error> {
        let keypair = node_key.into_keypair().map_err(|e| Error::Io(e))?;
        let local_peer_id = PeerId::from(keypair.public());
        info!(
            target: "sub-libp2p",
            "🏷 Local node identity is: {}",
            local_peer_id.to_base58(),
        );

        let transport = {
            let dh_keys = NoiseKeypair::<X25519Spec>::new()
                .into_authentic(&keypair)
                .expect("Noise key generation failed");

            TcpConfig::new()
                .upgrade(upgrade::Version::V1)
                .authenticate(NoiseConfig::xx(dh_keys).into_authenticated())
                .multiplex(mplex::MplexConfig::new())
                .boxed()
        };

        let mut request_response_protocols = vec![];

        for rc in params.rooms.clone() {
            let protocol_id = Cow::Owned(rc.id.protocol_name().to_string());
            let protocol_cfg =
                request_responses::ProtocolConfig::new(protocol_id, rc.inbound_queue);

            request_response_protocols.push(protocol_cfg);
        }

        let mut swarm = {
            let behaviour = {
                match Behaviour::new(&keypair, request_response_protocols, params.clone()) {
                    Ok(b) => b,
                    Err(request_responses::RegisterError::DuplicateProtocol(proto)) => {
                        return Err(Error::DuplicateBroadcastProtocol { protocol: proto });
                    }
                }
            };
            SwarmBuilder::with_async_std_executor(transport, behaviour, local_peer_id).build()
        };

        // Listen on the addresses.
        if let Err(err) = swarm.listen_on(params.listen_address) {
            warn!(target: "sub-libp2p", "Can't listen on 'listen_address' because: {:?}", err)
        }

        let (to_worker, from_service) = unbounded();

        let service = NetworkService {
            local_peer_id,
            to_worker,
        };

        let worker = NetworkWorker {
            local_peer_id,
            network_service: swarm,
            from_service,
            sessions: SessionManager::default(),
            service: Arc::new(service),
        };

        Ok(worker)
    }

    /// Return a `NetworkService` that can be shared through the code base and can be used to
    /// manipulate the worker.
    pub fn service(&self) -> &NetworkService {
        &self.service
    }

    /// Starts the libp2p service networking stack.
    pub async fn run(mut self) {
        // Bootstrap with Kademlia
        if let Err(e) = self.network_service.behaviour_mut().bootstrap() {
            warn!("Failed to bootstrap with Kademlia: {}", e);
        }

        let mut sessions = self.sessions;
        let mut swarm_stream = self.network_service.fuse();
        let mut network_stream = self.from_service.fuse();

        loop {
            select! {
                swarm_event = swarm_stream.next() => match swarm_event {
                    // Outbound events
                    Some(event) => match event {
                        SwarmEvent::Behaviour(behaviour_event) => match behaviour_event {
                            BehaviourEvent::InboundRequest { peer, protocol, result } => {
                                info!("Inbound message from {:?} related to {:?} protocol result {:?}", peer, protocol, result);
                            }
                            BehaviourEvent::RequestResponse(request_responses::Event::RequestFinished { peer, protocol, duration, result }) => {
                                debug!(
                                    "broadcast for protocol {:?} finished with {:?} peer: {:?} took: {:?}",
                                    protocol.to_string(),
                                    result,
                                    peer,
                                    duration
                                );
                            }
                            BehaviourEvent::Identify(event) => Behaviour::on_identify_event(&event),
                            BehaviourEvent::Ping(event) => Behaviour::on_ping_event(&event),
                            BehaviourEvent::Discovery(_) | BehaviourEvent::RequestResponse(_) => {}
                        },
                        SwarmEvent::NewListenAddr { address, .. } => info!("Listening on {:?}", address),
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            debug!(target: "sub-libp2p", "Libp2p => Connected({:?})", peer_id);
                        },
                        SwarmEvent::ConnectionClosed { peer_id: _, .. } => { }
                        _ => continue
                    }
                    None => { break; }
                },
                rpc_message = network_stream.next() => match rpc_message {
                    // Inbound requests
                    Some(request) => {
                        let behaviour = swarm_stream.get_mut().behaviour_mut();

                        match request {
                            ServicetoWorkerMsg::OpenRoom { room_id, max_size, respond_to } => {
                                let result = sessions.claim_or_create(behaviour, room_id, max_size);
                                let _ = respond_to.send(result);
                            }
                            ServicetoWorkerMsg::Request {
                                room_id,
                                context,
                                message
                            } => {
                                match message {
                                    MessageIntent::Broadcast(payload, response_sender) => {
                                        let peers: Vec<_> = swarm_stream.get_mut().connected_peers().cloned().collect();
                                        println!("NetworkService: Broadcasting to {} peers in room {:?}", peers.len(), room_id);
                                        for peer in &peers {
                                            println!("NetworkService: Sending to peer {}", peer);
                                        }
                                        behaviour.broadcast_message(
                                            peers.into_iter(),
                                            payload,
                                            room_id,
                                            context,
                                            response_sender,
                                            IfDisconnected::TryConnect,
                                        )
                                    }
                                    MessageIntent::Multicast(peer_ids, payload, response_sender) => {
                                        behaviour.broadcast_message(
                                            peer_ids.into_iter(),
                                            payload,
                                            room_id,
                                            context,
                                            response_sender,
                                            IfDisconnected::TryConnect,
                                        )
                                    }
                                    MessageIntent::SendDirect(peer_id, payload, response_sender) => {
                                        behaviour.send_request(
                                            &peer_id,
                                            payload,
                                            room_id,
                                            context,
                                            response_sender,
                                            IfDisconnected::TryConnect,
                                        )
                                    }
                                }
                            }
                        }
                    }
                    None => { break; }
                }
            };
        }
    }
}

impl NetworkService {
    pub async fn open_room(
        &self,
        room_id: RoomId,
        max_size: usize,
    ) -> Result<mpsc::Receiver<request_responses::IncomingRequest>, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.to_worker
            .send(ServicetoWorkerMsg::OpenRoom {
                room_id,
                max_size,
                respond_to: tx,
            })
            .await
            .expect("expected worker worker channel to not be full");

        rx.await.map_err(|_| SessionError::ChannelClosed)??
    }

    pub async fn broadcast_message(
        &self,
        room_id: &RoomId,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
    ) {
        self.to_worker
            .send(ServicetoWorkerMsg::Request {
                room_id: room_id.clone(),
                context,
                message: MessageIntent::Broadcast(payload, response_sender),
            })
            .await
            .expect("expected worker worker channel to not be full");
    }

    pub async fn broadcast_message_owned(
        self,
        room_id: RoomId,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
    ) {
        self.to_worker
            .send(ServicetoWorkerMsg::Request {
                room_id,
                context,
                message: MessageIntent::Broadcast(payload, response_sender),
            })
            .await
            .expect("expected worker worker channel to not be full");
    }

    pub async fn multicast_message(
        &self,
        room_id: &RoomId,
        peer_ids: impl Iterator<Item = PeerId>,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
    ) {
        self.to_worker
            .send(ServicetoWorkerMsg::Request {
                room_id: room_id.clone(),
                context,
                message: MessageIntent::Multicast(peer_ids.collect(), payload, response_sender),
            })
            .await
            .expect("expected worker worker channel to not be full");
    }

    pub async fn multicast_message_owned(
        self,
        room_id: RoomId,
        peer_ids: impl Iterator<Item = PeerId>,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
    ) {
        self.to_worker
            .send(ServicetoWorkerMsg::Request {
                room_id,
                context,
                message: MessageIntent::Multicast(peer_ids.collect(), payload, response_sender),
            })
            .await
            .expect("expected worker worker channel to not be full");
    }

    pub async fn send_message(
        &self,
        room_id: &RoomId,
        peer_id: PeerId,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>,
    ) {
        self.to_worker
            .send(ServicetoWorkerMsg::Request {
                room_id: room_id.clone(),
                context,
                message: MessageIntent::SendDirect(peer_id, payload, response_sender),
            })
            .await
            .expect("expected worker worker channel to not be full");
    }

    pub async fn send_message_owned(
        self,
        room_id: RoomId,
        peer_id: PeerId,
        context: MessageContext,
        payload: Vec<u8>,
        response_sender: mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>,
    ) {
        self.to_worker
            .send(ServicetoWorkerMsg::Request {
                room_id,
                context,
                message: MessageIntent::SendDirect(peer_id, payload, response_sender),
            })
            .await
            .expect("expected worker worker channel to not be full");
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id.clone()
    }
}
