use crate::adapter::{RuntimeIncoming, RuntimeOutgoing};
use anyhow::{anyhow, Context};
use mpc_protocols_codecs as codecs;
use mpc_protocols_frost::{run_signing, KeyShare};
use mpc_runtime::{IncomingRequest, OutgoingResponse, Peerset};
use std::{collections::BTreeSet, fs};

pub struct KeySign {
    path: String,
}

#[async_trait::async_trait]
impl mpc_runtime::ComputeAgentAsync for KeySign {
    fn protocol_id(&self) -> u64 {
        1
    }

    async fn compute(
        mut self: Box<Self>,
        mut parties: Peerset,
        payload: Vec<u8>,
        rt_incoming: async_channel::Receiver<IncomingRequest>,
        rt_outgoing: async_channel::Sender<OutgoingResponse>,
    ) -> anyhow::Result<Vec<u8>> {
        parties.recover_from_cache().await?;
        let i = parties.index_of(parties.local_peer_id()).unwrap() + 1;
        let signing_participants: Vec<u16> = parties
            .parties
            .iter()
            .map(|idx| (*idx + 1) as u16)
            .collect();

        println!("Signing participants: {:?}", signing_participants);
        println!("Current identifier: {}", i);

        let key_share = self.read_key_share()?;
        println!("Key share identifier: {}", key_share.identifier);

        if i != key_share.identifier {
            return Err(anyhow!(
                "Current identifier {} does not match key share identifier {}. \
                This node may not have participated in the original keygen.",
                i,
                key_share.identifier
            ));
        }

        let mut protocol_incoming = RuntimeIncoming::new(rt_incoming);
        let mut protocol_outgoing = RuntimeOutgoing::new(rt_outgoing);

        let signature = match key_share.curve {
            mpc_network::Curve::Ed25519 => {
                run_signing::<frost_ed25519::Ed25519Sha512, _, _>(
                    &key_share,
                    i,
                    &signing_participants,
                    &payload,
                    &mut protocol_incoming,
                    &mut protocol_outgoing,
                )
                .await?
            }
            mpc_network::Curve::Secp256k1 => {
                run_signing::<frost_secp256k1::Secp256K1Sha256, _, _>(
                    &key_share,
                    i,
                    &signing_participants,
                    &payload,
                    &mut protocol_incoming,
                    &mut protocol_outgoing,
                )
                .await?
            }
        };
        let signature_bytes =
            codecs::encode(&signature).map_err(|e| anyhow!("error encoding signature {e}"))?;

        Ok(signature_bytes)
    }
}

impl KeySign {
    pub fn new(p: &str) -> Self {
        Self { path: p.to_owned() }
    }

    fn read_key_share(&self) -> anyhow::Result<KeyShare> {
        let share_bytes = fs::read(&self.path).context("failed to read local key")?;
        let key_share: KeyShare =
            serde_json::from_slice(&share_bytes).context("failed to deserialize local key")?;

        let expected_ids: BTreeSet<u16> = (1..=key_share.max_signers).collect();
        if !expected_ids.contains(&key_share.identifier) {
            return Err(anyhow!(
                "key share identifier {} is not in expected range 1..={} for this key",
                key_share.identifier,
                key_share.max_signers
            ));
        }

        Ok(key_share)
    }
}
