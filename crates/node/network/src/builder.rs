use crate::{MultiaddrWithPeerId, Params, RoomConfig};
use libp2p::Multiaddr;

/// Builder for composing [`Params`] in a more extensible way.
///
/// The existing [`Params`] struct mirrors the simple configuration used by the
/// initial prototype. Introducing a builder makes it easier to grow the
/// networking stack (e.g. adding more request/response rooms, changing
/// discovery strategies, or injecting additional boot nodes) without having to
/// touch call sites spread across the codebase.
#[derive(Default)]
pub struct NetworkParamsBuilder {
    listen_address: Option<Multiaddr>,
    mdns: bool,
    kademlia: bool,
    boot_nodes: Vec<MultiaddrWithPeerId>,
    rooms: Vec<RoomConfig>,
}

impl NetworkParamsBuilder {
    /// Start building params using the provided listen address.
    pub fn new(listen_address: Multiaddr) -> Self {
        Self {
            listen_address: Some(listen_address),
            ..Default::default()
        }
    }

    /// Enable or disable mDNS discovery.
    pub fn with_mdns(mut self, enabled: bool) -> Self {
        self.mdns = enabled;
        self
    }

    /// Enable or disable Kademlia discovery.
    pub fn with_kademlia(mut self, enabled: bool) -> Self {
        self.kademlia = enabled;
        self
    }

    /// Append additional boot nodes that will be dialed eagerly when the
    /// network starts. This makes it easier to scale horizontally by
    /// introducing more seed nodes.
    pub fn with_boot_nodes(
        mut self,
        boot_nodes: impl IntoIterator<Item = MultiaddrWithPeerId>,
    ) -> Self {
        self.boot_nodes.extend(boot_nodes);
        self
    }

    /// Attach a room configuration. Callers can register multiple rooms to
    /// allow independent request/response channels per computation cohort.
    pub fn with_room(mut self, room: RoomConfig) -> Self {
        self.rooms.push(room);
        self
    }

    /// Finalize the builder and return [`Params`].
    pub fn build(self) -> Params {
        let listen_address = self
            .listen_address
            .expect("listen address must be provided before building network params");

        Params {
            listen_address,
            mdns: self.mdns,
            kademlia: self.kademlia,
            rooms: self.rooms,
            boot_nodes: self.boot_nodes,
        }
    }
}
