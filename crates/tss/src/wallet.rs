use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_WALLET_ID: &str = "default";
pub const DEFAULT_TRANSPORT_ROOM: &str = "tss/0";
const MAX_WALLET_ID_LEN: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeygenRequestPayload {
    pub wallet_id: String,
    pub threshold: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignRequestPayload {
    pub wallet_id: String,
    pub message: Vec<u8>,
}

pub fn normalize_wallet_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_WALLET_ID.to_owned();
    }

    let mut sanitized = String::with_capacity(trimmed.len().min(MAX_WALLET_ID_LEN));
    for ch in trimmed.chars() {
        if sanitized.len() >= MAX_WALLET_ID_LEN {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }

    if sanitized.is_empty() {
        DEFAULT_WALLET_ID.to_owned()
    } else {
        sanitized
    }
}

pub fn encode_keygen_payload(wallet_id: &str, threshold: u16) -> anyhow::Result<Vec<u8>> {
    let payload = KeygenRequestPayload {
        wallet_id: normalize_wallet_id(wallet_id),
        threshold,
    };
    serde_ipld_dagcbor::to_vec(&payload)
        .map_err(|e| anyhow!("failed to encode keygen wallet payload: {e}"))
}

pub fn decode_keygen_payload(payload: &[u8]) -> anyhow::Result<KeygenRequestPayload> {
    let decoded = serde_ipld_dagcbor::from_slice::<KeygenRequestPayload>(payload)
        .map_err(|e| anyhow!("failed to decode keygen wallet payload: {e}"))?;
    Ok(KeygenRequestPayload {
        wallet_id: normalize_wallet_id(&decoded.wallet_id),
        threshold: decoded.threshold,
    })
}

pub fn encode_sign_payload(wallet_id: &str, message: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    let payload = SignRequestPayload {
        wallet_id: normalize_wallet_id(wallet_id),
        message,
    };
    serde_ipld_dagcbor::to_vec(&payload)
        .map_err(|e| anyhow!("failed to encode sign wallet payload: {e}"))
}

pub fn decode_sign_payload(payload: &[u8]) -> anyhow::Result<SignRequestPayload> {
    let decoded = serde_ipld_dagcbor::from_slice::<SignRequestPayload>(payload)
        .map_err(|e| anyhow!("failed to decode sign wallet payload: {e}"))?;
    Ok(SignRequestPayload {
        wallet_id: normalize_wallet_id(&decoded.wallet_id),
        message: decoded.message,
    })
}

pub fn wallet_key_share_path(base_path: &str, wallet_id: &str) -> PathBuf {
    let wallet_id = normalize_wallet_id(wallet_id);
    let base = Path::new(base_path);

    if base.extension().is_some() {
        let parent = base.parent().unwrap_or_else(|| Path::new("."));
        parent.join("wallets").join(format!("{wallet_id}.share"))
    } else {
        base.join(format!("{wallet_id}.share"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_wallet_id_replaces_unsafe_chars() {
        assert_eq!(normalize_wallet_id("wallet/alpha"), "wallet_alpha");
        assert_eq!(normalize_wallet_id(" "), DEFAULT_WALLET_ID);
    }

    #[test]
    fn wallet_key_share_path_uses_wallets_dir_for_default_wallet() {
        let path = wallet_key_share_path("data/node/key.share", DEFAULT_WALLET_ID);
        assert_eq!(path, Path::new("data/node/wallets/default.share"));
    }

    #[test]
    fn wallet_key_share_path_uses_wallets_dir_for_non_default_wallet() {
        let path = wallet_key_share_path("data/node/key.share", "wallet-1");
        assert_eq!(path, Path::new("data/node/wallets/wallet-1.share"));
    }

    #[test]
    fn decode_keygen_payload_rejects_invalid_format() {
        let invalid_payload = vec![0x01, 0x02, 0x03];
        assert!(decode_keygen_payload(&invalid_payload).is_err());
    }

    #[test]
    fn decode_sign_payload_rejects_invalid_format() {
        let invalid_payload = b"hello world".to_vec();
        assert!(decode_sign_payload(&invalid_payload).is_err());
    }
}
