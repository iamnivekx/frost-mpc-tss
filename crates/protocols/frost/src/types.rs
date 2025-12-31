use mpc_network::Curve;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KeyShare {
    pub curve: Curve,
    pub identifier: u16,
    pub signing_key: Vec<u8>,
    pub public_key: Vec<u8>,
    pub group_public_key: Vec<u8>,
    pub public_key_package: Vec<u8>,
    pub min_signers: u16,
    pub max_signers: u16,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PublicKey {
    pub curve: Curve,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Signature {
    pub curve: Curve,
    pub pub_key: Vec<u8>,
    pub signature: Vec<u8>,
}
