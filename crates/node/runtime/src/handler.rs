use crate::peerset::Peerset;

use futures::channel::oneshot;
use mpc_network::RoomId;

pub struct IncomingRequest {
    /// Index of party who sent the message.
    pub from: u16,

    /// Request sent by the remote. Will always be smaller than
    /// [`ProtocolConfig::max_request_size`].
    pub payload: Vec<u8>,

    pub to: Option<u16>,
}

pub struct OutgoingResponse {
    /// Message sent by the remote.
    pub body: Vec<u8>,

    pub to: Option<u16>,

    pub sent_feedback: Option<oneshot::Sender<()>>,
}

pub trait ProtocolAgentFactory {
    fn make(&self, protocol_id: u64) -> crate::Result<Box<dyn ComputeAgentAsync>>;
    fn keygen(&self) -> Box<dyn ComputeAgentAsync>;
    fn keysign(&self) -> Box<dyn ComputeAgentAsync>;
}

#[async_trait::async_trait]
pub trait ComputeAgentAsync: Send + Sync {
    fn protocol_id(&self) -> u64;

    async fn compute(
        self: Box<Self>,
        parties: Peerset,
        payload: Vec<u8>,
        incoming: async_channel::Receiver<IncomingRequest>,
        outgoing: async_channel::Sender<OutgoingResponse>,
    ) -> anyhow::Result<Vec<u8>>;
}

pub trait PeersetStorage {
    fn read_peerset(&self, room_id: &RoomId) -> anyhow::Result<Peerset>;
    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> anyhow::Result<()>;
}
