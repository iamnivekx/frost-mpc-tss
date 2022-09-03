use crate::discovery::{DiscoveryBehaviour, DiscoveryOut};
use crate::request_responses::ResponseFailure;
use crate::{request_responses, request_responses::MessageContext, Params, RoomId};
use futures::channel::mpsc;
use libp2p::identify::{Identify, IdentifyConfig, IdentifyEvent};
use libp2p::identity::Keypair;
use libp2p::kad::QueryId;
use libp2p::ping::{Ping, PingEvent, PingFailure, PingSuccess};
use libp2p::swarm::NetworkBehaviourEventProcess;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use libp2p::NetworkBehaviour;
use libp2p::PeerId;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::task::{Context, Poll};
use std::time::Duration;
use tracing::{debug, trace};

const MPC_PROTOCOL_ID: &str = "/mpc/0.1.0";

/// General behaviour of the network. Combines all protocols together.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "BehaviourOut", poll_method = "poll", event_process = true)]
pub struct Behaviour {
    ping: Ping,
    identify: Identify,
    discovery: DiscoveryBehaviour,
    /// Handles multiple communication of multiple generic protocols.
    /// Generic request-response protocols.
    request_responses: request_responses::RequestResponsesBehaviour,

    #[behaviour(ignore)]
    events: VecDeque<BehaviourOut>,
}

pub enum BehaviourOut {
    InboundRequest {
        /// Peer which sent us a request.
        peer: PeerId,
        /// Protocol name of the request.
        protocol: Cow<'static, str>,
        /// If `Ok`, contains the time elapsed between when we received the request and when we
        /// sent back the response. If `Err`, the error that happened.
        result: Result<Duration, ResponseFailure>,
    },
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
            identify: Identify::new(IdentifyConfig::new(
                MPC_PROTOCOL_ID.into(),
                local_key.public(),
            )),
            ping: Ping::default(),
            events: VecDeque::new(),
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

    /// Bootstrap Kademlia network.
    pub fn bootstrap(&mut self) -> Result<QueryId, String> {
        self.discovery.bootstrap()
    }

    /// Known peers.
    pub fn peers(&self, _room_id: RoomId) -> impl Iterator<Item = PeerId> {
        self.discovery.peers().clone().into_iter()
    }

    /// Consumes the events list when polled.
    fn poll(
        &mut self,
        _: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<BehaviourOut, <Self as NetworkBehaviour>::ProtocolsHandler>>
    {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event));
        }

        Poll::Pending
    }
}

impl NetworkBehaviourEventProcess<request_responses::Event> for Behaviour {
    fn inject_event(&mut self, event: request_responses::Event) {
        match event {
            request_responses::Event::InboundRequest {
                peer,
                protocol,
                result,
            } => {
                self.events.push_back(BehaviourOut::InboundRequest {
                    peer,
                    protocol,
                    result,
                });
            }
            request_responses::Event::RequestFinished {
                peer,
                protocol,
                duration,
                result,
            } => {
                debug!(
                    "broadcast for protocol {:?} finished with {:?} peer: {:?} took: {:?}",
                    protocol.to_string(),
                    result,
                    peer,
                    duration
                );
            }
        }
    }
}

impl NetworkBehaviourEventProcess<DiscoveryOut> for Behaviour {
    fn inject_event(&mut self, event: DiscoveryOut) {
        match event {
            DiscoveryOut::Connected(..) => {}
            DiscoveryOut::Disconnected(..) => {}
        }
    }
}

impl NetworkBehaviourEventProcess<IdentifyEvent> for Behaviour {
    fn inject_event(&mut self, event: IdentifyEvent) {
        match event {
            IdentifyEvent::Received { peer_id, info } => {
                trace!("identified peer {:?}", peer_id);
                trace!("protocol_version {:?}", info.protocol_version);
                trace!("agent_version {:?}", info.agent_version);
                trace!("listen_addresses {:?}", info.listen_addrs);
                trace!("observed_address {:?}", info.observed_addr);
                trace!("protocols {:?}", info.protocols);
            }
            IdentifyEvent::Sent { .. } => (),
            IdentifyEvent::Pushed { .. } => (),
            IdentifyEvent::Error { .. } => (),
        }
    }
}

impl NetworkBehaviourEventProcess<PingEvent> for Behaviour {
    fn inject_event(&mut self, event: PingEvent) {
        match event.result {
            Ok(PingSuccess::Ping { rtt }) => {
                trace!(
                    "PingSuccess::Ping rtt to {} is {} ms",
                    event.peer.to_base58(),
                    rtt.as_millis()
                );
            }
            Ok(PingSuccess::Pong) => {
                trace!("PingSuccess::Pong from {}", event.peer.to_base58());
            }
            Err(PingFailure::Timeout) => {
                debug!("PingFailure::Timeout {}", event.peer.to_base58());
            }
            Err(PingFailure::Other { error }) => {
                debug!("PingFailure::Other {}: {}", event.peer.to_base58(), error);
            }
            Err(PingFailure::Unsupported) => {
                debug!("PingFailure::Unsupported {}", event.peer.to_base58());
            }
        }
    }
}
