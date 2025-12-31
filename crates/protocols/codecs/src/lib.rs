use anyhow::Context;
use serde::{de::DeserializeOwned, Serialize};

pub fn encode<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    serde_ipld_dagcbor::to_vec(value).context("failed to encode dag-cbor payload")
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
    serde_ipld_dagcbor::from_slice(bytes).context("failed to decode dag-cbor payload")
}
