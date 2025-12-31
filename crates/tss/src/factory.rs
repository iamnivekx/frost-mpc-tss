use crate::keysign::KeySign;
use crate::KeyGen;
use mpc_network::Curve;
use mpc_service::ComputeAgentAsync;

pub struct TssFactory {
    key_path: String,
    curve: Curve,
}

impl TssFactory {
    pub fn new(key_path: String, curve: Curve) -> Self {
        Self { key_path, curve }
    }
}

impl mpc_service::ProtocolAgentFactory for TssFactory {
    fn make(&self, protocol_id: u64) -> mpc_service::Result<Box<dyn ComputeAgentAsync>> {
        match protocol_id {
            0 => Ok(Box::new(KeyGen::new(&self.key_path, self.curve))),
            1 => Ok(Box::new(KeySign::new(&self.key_path))),
            _ => Err(mpc_service::Error::UnknownProtocol(protocol_id)),
        }
    }
    fn keysign(&self) -> Box<dyn ComputeAgentAsync> {
        Box::new(KeySign::new(&self.key_path))
    }
    fn keygen(&self) -> Box<dyn ComputeAgentAsync> {
        Box::new(KeyGen::new(&self.key_path, self.curve))
    }
}
