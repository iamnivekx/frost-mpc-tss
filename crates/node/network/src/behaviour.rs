use crate::discovery::{DiscoveryBehaviour, DiscoveryOut};
use crate::request_responses::ResponseFailure;
use crate::{request_responses, request_responses::MessageContext, Params, RoomId};
use futures::channel::mpsc;
use libp2p::identify;
use libp2p_identity::Keypair;
use libp2p_kad::QueryId;
use libp2p::ping;
use libp2p::swarm::NetworkBehaviour;
use libp2p::PeerId;
use std::borrow::Cow;
use std::time::Duration;
use tracing::{debug, trace};

const MPC_PROTOCOL_ID: &str = "/mpc/0.1.0";

/// General behaviour of the network. Combines all protocols together.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BehaviourEvent")]
pub struct Behaviour {
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    discovery: DiscoveryBehaviour,
    /// Handles multiple communication of multiple generic protocols.
    /// Generic request-response protocols.
    request_responses: request_responses::RequestResponsesBehaviour,
}

#[derive(Debug)]
pub enum BehaviourEvent {
    InboundRequest {
        /// Peer which sent us a request.
        peer: PeerId,
        /// Protocol name of the request.
        protocol: Cow<'static, str>,
        /// If `Ok`, contains the time elapsed between when we received the request and when we
        /// sent back the response. If `Err`, the error that happened.
        result: Result<Duration, ResponseFailure>,
    },
    RequestResponse(request_responses::Event),
    Discovery(DiscoveryOut),
    Identify(identify::Event),
    Ping(ping::Event),
}

impl From<request_responses::Event> for BehaviourEvent {
    fn from(event: request_responses::Event) -> Self {
        match &event {
            request_responses::Event::InboundRequest {
                peer,
                protocol,
                result,
            } => BehaviourEvent::InboundRequest {
                peer: *peer,
                protocol: protocol.clone(),
                result: result.clone(),
            },
            _ => BehaviourEvent::RequestResponse(event),
        }
    }
}

impl From<DiscoveryOut> for BehaviourEvent {
    fn from(event: DiscoveryOut) -> Self {
        BehaviourEvent::Discovery(event)
    }
}

impl From<identify::Event> for BehaviourEvent {
    fn from(event: identify::Event) -> Self {
        BehaviourEvent::Identify(event)
    }
}

impl From<ping::Event> for BehaviourEvent {
    fn from(event: ping::Event) -> Self {
        BehaviourEvent::Ping(event)
    }
}

impl Behaviour {
    pub fn new(
        local_key: &Keypair,
        request_response_protocols: Vec<request_responses::ProtocolConfig>,
        params: Params,
    ) -> Result<Behaviour, request_responses::RegisterError> {
        Ok(Behaviour {
            request_responses: request_responses::RequestResponsesBehaviour::new(
                request_response_protocols.into_iter(),
            )?,
            discovery: DiscoveryBehaviour::new(local_key.public(), params),
            identify: identify::Behaviour::new(identify::Config::new(
                MPC_PROTOCOL_ID.into(),
                local_key.public(),
            )),
            ping: ping::Behaviour::default(),
        })
    }

    /// Initiates direct sending of a message.
    pub fn send_request(
        &mut self,
        target: &PeerId,
        request: Vec<u8>,
        room_id: RoomId,
        ctx: MessageContext,
        pending_response: mpsc::Sender<
            Result<(PeerId, Vec<u8>), request_responses::RequestFailure>,
        >,
        connect: request_responses::IfDisconnected,
    ) {
        self.request_responses.send_request(
            target,
            &room_id.protocol_name(),
            ctx,
            request,
            pending_response,
            connect,
        )
    }

    /// Initiates broadcasting of a message.
    pub fn broadcast_message(
        &mut self,
        targets: impl Iterator<Item = PeerId>,
        payload: Vec<u8>,
        room_id: RoomId,
        ctx: MessageContext,
        pending_response: Option<
            mpsc::Sender<Result<(PeerId, Vec<u8>), request_responses::RequestFailure>>,
        >,
        connect: request_responses::IfDisconnected,
    ) {
        self.request_responses.broadcast_message(
            targets,
            &room_id.protocol_name(),
            ctx,
            payload,
            pending_response,
            connect,
        );
    }

    pub fn register_room(
        &mut self,
        room_id: RoomId,
        inbound_queue: mpsc::Sender<request_responses::IncomingRequest>,
    ) -> Result<(), request_responses::RegisterError> {
        let protocol_id = room_id.protocol_name();
        self.request_responses
            .register_protocol(request_responses::ProtocolConfig::new(
                protocol_id,
                Some(inbound_queue),
            ))
    }

    /// Bootstrap Kademlia network.
    pub fn bootstrap(&mut self) -> Result<QueryId, String> {
        self.discovery.bootstrap()
    }

    pub fn on_identify_event(event: &identify::Event) {
        if let identify::Event::Received { peer_id, info } = event {
            trace!("identified peer {:?}", peer_id);
            trace!("protocol_version {:?}", info.protocol_version);
            trace!("agent_version {:?}", info.agent_version);
            trace!("listen_addresses {:?}", info.listen_addrs);
            trace!("observed_address {:?}", info.observed_addr);
            trace!("protocols {:?}", info.protocols);
        }
    }

    pub fn on_ping_event(event: &ping::Event) {
        match event.result {
            Ok(ping::Success::Ping { rtt }) => {
                trace!(
                    "PingSuccess::Ping rtt to {} is {} ms",
                    event.peer.to_base58(),
                    rtt.as_millis()
                );
            }
            Ok(ping::Success::Pong) => {
                trace!("PingSuccess::Pong from {}", event.peer.to_base58());
            }
            Err(ping::Failure::Timeout) => {
                debug!("PingFailure::Timeout {}", event.peer.to_base58());
            }
            Err(ping::Failure::Other { error }) => {
                debug!("PingFailure::Other {}: {}", event.peer.to_base58(), error);
            }
            Err(ping::Failure::Unsupported) => {
                debug!("PingFailure::Unsupported {}", event.peer.to_base58());
            }
        }
    }
}
