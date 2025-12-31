use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, strum::Display)]
pub enum Curve {
    #[strum(serialize = "ed25519")]
    #[serde(rename = "ed25519")]
    Ed25519,
    #[strum(serialize = "secp256k1")]
    #[serde(rename = "secp256k1")]
    Secp256k1,
}
