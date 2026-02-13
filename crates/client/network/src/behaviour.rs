use crate::{
    discovery::{DiscoveryBehaviour, DiscoveryConfig, DiscoveryOut},
    peer_store::MockPeerStore,
    request_responses,
    request_responses::MessageContext,
    Params, RoomId,
};
use futures::channel::mpsc;
use libp2p::identify::{Behaviour as Identify, Config as IdentifyConfig};
use libp2p::identity::Keypair;
use libp2p::kad::QueryId;
use libp2p::ping::Behaviour as Ping;
use libp2p::swarm::NetworkBehaviour;
use libp2p::PeerId;
use std::sync::Arc;

const MPC_PROTOCOL_ID: &str = "/mpc/0.1.0";

/// General behaviour of the network. Combines all protocols together.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BehaviourOut")]
pub struct Behaviour {
    ping: Ping,
    identify: Identify,
    discovery: DiscoveryBehaviour,
    /// Handles multiple communication of multiple generic protocols.
    /// Generic request-response protocols.
    request_responses: request_responses::RequestResponsesBehaviour,
}

pub enum BehaviourOut {
    RequestResponse(request_responses::Event),
    Discovery(DiscoveryOut),
    Identify(libp2p::identify::Event),
    Ping(libp2p::ping::Event),
}

impl Behaviour {
    pub fn new(
        local_key: &Keypair,
        request_response_protocols: Vec<request_responses::ProtocolConfig>,
        params: Params,
    ) -> Result<Behaviour, request_responses::RegisterError> {
        let local_peer_id = local_key.public().to_peer_id();

        // Build DiscoveryConfig from Params
        let permanent_addresses: Vec<_> = params
            .rooms
            .iter()
            .flat_map(|ra| ra.boot_nodes.clone())
            .map(|mwp| (mwp.peer_id, mwp.multiaddr))
            .collect();

        let mut discovery_config = DiscoveryConfig::new(local_peer_id);
        discovery_config
            .with_permanent_addresses(permanent_addresses)
            .with_mdns(params.mdns);

        if params.kademlia {
            discovery_config.with_kademlia(libp2p::swarm::StreamProtocol::new("/ipfs/kad/1.0.0"));
        }

        Ok(Behaviour {
            request_responses: request_responses::RequestResponsesBehaviour::new(
                request_response_protocols.into_iter(),
                Arc::new(MockPeerStore),
            )?,
            discovery: discovery_config.finish(),
            identify: Identify::new(IdentifyConfig::new(
                MPC_PROTOCOL_ID.into(),
                local_key.public(),
            )),
            ping: Ping::new(Default::default()),
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
        self.request_responses.send_request_with_context(
            target,
            &room_id.protocol_name(),
            ctx,
            request,
            false,
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
    pub fn peers(&mut self, _room_id: RoomId) -> impl Iterator<Item = PeerId> {
        self.discovery.known_peers().into_iter()
    }
}

impl From<request_responses::Event> for BehaviourOut {
    fn from(event: request_responses::Event) -> Self {
        BehaviourOut::RequestResponse(event)
    }
}

impl From<DiscoveryOut> for BehaviourOut {
    fn from(_event: DiscoveryOut) -> Self {
        BehaviourOut::Discovery(_event)
    }
}

impl From<libp2p::identify::Event> for BehaviourOut {
    fn from(event: libp2p::identify::Event) -> Self {
        BehaviourOut::Identify(event)
    }
}

impl From<libp2p::ping::Event> for BehaviourOut {
    fn from(event: libp2p::ping::Event) -> Self {
        BehaviourOut::Ping(event)
    }
}
