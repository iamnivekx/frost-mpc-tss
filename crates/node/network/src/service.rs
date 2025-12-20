use crate::{
    behaviour::{Behaviour, BehaviourOut},
    config::MultiaddrWithPeerId,
    error::Error,
    request_responses,
    request_responses::IfDisconnected,
    request_responses::MessageContext,
    request_responses::RequestFailure,
    NodeKeyConfig, RoomId,
};
use async_channel::{unbounded, Receiver, Sender};
use futures::channel::{mpsc, oneshot};
use futures_util::stream::StreamExt;
use libp2p::core::transport::upgrade;
use libp2p::noise;
use libp2p::swarm::SwarmEvent;
use libp2p::{tcp, yamux, PeerId, Swarm, Transport};
use std::time::Duration;
use std::{borrow::Cow, collections::HashSet, sync::Arc};
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
    GetConnectedPeers {
        response: oneshot::Sender<HashSet<PeerId>>,
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
    /// The boot nodes to connect to.
    boot_nodes: Vec<MultiaddrWithPeerId>,
    /// Track connected peers
    connected_peers: HashSet<PeerId>,
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
        let local_peer_id = keypair.public().to_peer_id();
        info!(
            target: "sub-libp2p",
            "🏷 Local node identity is: {}",
            local_peer_id.to_string(),
        );

        let transport = {
            let noise_config = noise::Config::new(&keypair).expect("Noise key generation failed");

            tcp::tokio::Transport::new(tcp::Config::default().nodelay(true))
                .upgrade(upgrade::Version::V1)
                .authenticate(noise_config)
                .multiplex(yamux::Config::default())
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
            let config = libp2p::swarm::Config::with_tokio_executor()
                .with_idle_connection_timeout(std::time::Duration::from_secs(60));
            Swarm::new(transport, behaviour, local_peer_id, config)
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
            service: Arc::new(service),
            boot_nodes: params.boot_nodes,
            connected_peers: HashSet::new(),
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
        // Dial boot nodes to establish initial connections
        for boot_node in &self.boot_nodes {
            let peer_id = boot_node.peer_id;
            let addr = boot_node.concat();
            info!(target: "sub-libp2p", "Dialing boot node {} at {}", peer_id, addr);
            match self.network_service.dial(addr) {
                Ok(()) => {
                    debug!(target: "sub-libp2p", "Successfully initiated dial to boot node {}", peer_id);
                }
                Err(e) => {
                    warn!(target: "sub-libp2p", "Failed to dial boot node {}: {:?}", peer_id, e);
                }
            }
        }

        // Bootstrap with Kademlia (after dialing boot nodes)
        if let Err(e) = self.network_service.behaviour_mut().bootstrap() {
            warn!("Failed to bootstrap with Kademlia: {}", e);
        }

        let mut swarm_stream = self.network_service.fuse();
        let mut network_stream = self.from_service.fuse();
        let mut retry_interval = tokio::time::interval(Duration::from_secs(1)); // Retry dialing boot nodes every 1 second for faster connection
        let mut connected_boot_nodes = std::collections::HashSet::new();

        loop {
            tokio::select! {
                _ = retry_interval.tick() => {
                    // Periodically retry dialing boot nodes that aren't connected yet
                    for boot_node in &self.boot_nodes {
                        if !connected_boot_nodes.contains(&boot_node.peer_id) {
                            let peer_id = boot_node.peer_id;
                            let addr = boot_node.concat();
                            debug!(target: "sub-libp2p", "Retrying dial to boot node {} at {}", peer_id, addr);
                            match swarm_stream.get_mut().dial(addr) {
                                Ok(()) => {
                                    debug!(target: "sub-libp2p", "Successfully initiated retry dial to boot node {}", peer_id);
                                }
                                Err(e) => {
                                    debug!(target: "sub-libp2p", "Retry dial to boot node {} failed: {:?}", peer_id, e);
                                }
                            }
                        }
                    }
                }
                swarm_event = swarm_stream.next() => match swarm_event {
                    // Outbound events
                    Some(event) => match event {
                        SwarmEvent::Behaviour(BehaviourOut::RequestResponse(ev)) => {
                            match ev {
                                request_responses::Event::InboundRequest { peer, protocol, result } => {
                                    info!("Inbound message from {:?} related to {:?} protocol result {:?}", peer, protocol, result);
                                }
                                request_responses::Event::RequestFinished { peer, protocol, duration, result } => {
                                    debug!(
                                        "Request finished: protocol {:?} peer {:?} took {:?} result {:?}",
                                        protocol,
                                        peer,
                                        duration,
                                        result
                                    );
                                }
                            }
                        },
                        SwarmEvent::Behaviour(BehaviourOut::Discovery(_)) => {}
                        SwarmEvent::Behaviour(BehaviourOut::Identify(_)) => {}
                        SwarmEvent::Behaviour(BehaviourOut::Ping(_)) => {}
                        SwarmEvent::NewListenAddr { address, .. } => info!("Listening on {:?}", address),
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            info!(target: "sub-libp2p", "Libp2p => Connected({:?})", peer_id);
                            // Track all connected peers
                            self.connected_peers.insert(peer_id);
                            // Mark boot node as connected if it's in our boot nodes list
                            if self.boot_nodes.iter().any(|bn| bn.peer_id == peer_id) {
                                connected_boot_nodes.insert(peer_id);
                                debug!(target: "sub-libp2p", "Boot node {} is now connected", peer_id);
                            }
                        },
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            self.connected_peers.remove(&peer_id);
                            connected_boot_nodes.remove(&peer_id);
                        },
                        _ => continue
                    }
                    None => { break; }
                },
                rpc_message = network_stream.next() => match rpc_message {
                    // Inbound requests
                    Some(request) => {
                        let behaviour = swarm_stream.get_mut().behaviour_mut();

                        match request {
                            ServicetoWorkerMsg::GetConnectedPeers { response } => {
                                let _ = response.send(self.connected_peers.clone());
                            }
                            ServicetoWorkerMsg::Request {
                                room_id,
                                context,
                                message
                            } => {
                                match message {
                                    MessageIntent::Broadcast(payload, response_sender) => {
                                        let peers: Vec<_> = behaviour.peers(room_id).collect();
                                        println!("NetworkService: Broadcasting to {} peers in room {:?}", peers.len(), room_id);
                                        println!("NetworkService: All known peers at broadcast time: {:?}", peers);
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

    /// Get the set of currently connected peers
    pub async fn get_connected_peers(&self) -> HashSet<PeerId> {
        let (tx, rx) = oneshot::channel();
        self.to_worker
            .send(ServicetoWorkerMsg::GetConnectedPeers { response: tx })
            .await
            .expect("expected worker channel to not be full");
        rx.await.unwrap_or_default()
    }
}
