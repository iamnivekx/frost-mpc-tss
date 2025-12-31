use crate::keygen::KeyShare;
use anyhow::{anyhow, Context};
use frost_core::{
    keys::{KeyPackage, PublicKeyPackage, SigningShare, VerifyingShare},
    Ciphersuite, Identifier, SigningPackage,
};
use mpc_network::Curve;
use mpc_service::{IncomingRequest, OutgoingResponse, Peerset};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

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

pub struct KeySign {
    path: String,
}

#[async_trait::async_trait]
impl mpc_service::ComputeAgentAsync for KeySign {
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

        // Verify that current identifier matches key share identifier
        if i != key_share.identifier {
            return Err(anyhow!(
                "Current identifier {} does not match key share identifier {}. \
                This node may not have participated in the original keygen.",
                i,
                key_share.identifier
            ));
        }

        let signature = match key_share.curve {
            Curve::Ed25519 => {
                self.run_signing::<frost_ed25519::Ed25519Sha512>(
                    &key_share,
                    i,
                    &signing_participants,
                    &payload,
                    rt_incoming,
                    rt_outgoing,
                )
                .await?
            }
            Curve::Secp256k1 => {
                self.run_signing::<frost_secp256k1::Secp256K1Sha256>(
                    &key_share,
                    i,
                    &signing_participants,
                    &payload,
                    rt_incoming,
                    rt_outgoing,
                )
                .await?
            }
        };
        let signature_bytes = serde_ipld_dagcbor::to_vec(&signature)
            .map_err(|e| anyhow!("error encoding signature {e}"))?;

        Ok(signature_bytes)
    }
}

impl KeySign {
    pub fn new(p: &str) -> Self {
        Self { path: p.to_owned() }
    }

    async fn run_signing<C: Ciphersuite>(
        &self,
        key_share: &KeyShare,
        identifier: u16,
        signing_participants: &[u16],
        message: &[u8],
        rt_incoming: async_channel::Receiver<IncomingRequest>,
        rt_outgoing: async_channel::Sender<OutgoingResponse>,
    ) -> anyhow::Result<Signature> {
        // Use the identifier from the key share instead of the dynamic one
        // This ensures consistency between keygen and signing
        let key_share_identifier = key_share.identifier;
        println!(
            "Using key share identifier: {} (instead of dynamic identifier: {})",
            key_share_identifier, identifier
        );

        // Convert u16 identifier to Identifier<C>
        let identifier_id = Identifier::try_from(key_share_identifier)
            .map_err(|e| anyhow!("invalid identifier: {e}"))?;

        // Deserialize signing share and verifying share
        let signing_share = SigningShare::<C>::deserialize(&key_share.signing_key)
            .map_err(|e| anyhow!("failed to deserialize signing share: {e}"))?;
        let verifying_share = VerifyingShare::<C>::deserialize(&key_share.public_key)
            .map_err(|e| anyhow!("failed to deserialize verifying share: {e}"))?;
        let verifying_key = frost_core::VerifyingKey::<C>::deserialize(&key_share.group_public_key)
            .map_err(|e| anyhow!("failed to deserialize group public key: {e}"))?;

        // Use min_signers from KeyShare (stored during keygen)
        let min_signers = key_share.min_signers;
        let key_package = KeyPackage::new(
            identifier_id,
            signing_share,
            verifying_share,
            verifying_key,
            min_signers,
        );

        // Round 1: Generate nonces
        let (nonces, commitments) =
            frost_core::round1::commit(key_package.signing_share(), &mut rand::rngs::OsRng);

        // Send and receive commitments
        use frost_core::round1::SigningCommitments;
        let commitments_payload = commitments
            .serialize()
            .map_err(|e| anyhow!("failed to serialize commitments: {e}"))?;
        let commitments_payload_cbor = serde_ipld_dagcbor::to_vec(&commitments_payload)
            .map_err(|e| anyhow!("failed to encode commitments: {e}"))?;
        println!(
            "Round 1: Sending commitments, waiting for {} other participants",
            signing_participants.len() - 1
        );
        let (tx1, rx1) = futures::channel::oneshot::channel();
        rt_outgoing
            .send(mpc_service::OutgoingResponse {
                body: commitments_payload_cbor,
                to: None,
                sent_feedback: Some(tx1),
            })
            .await
            .map_err(|e| anyhow!("error sending commitments: {e}"))?;
        // Wait for message to be sent (written to buffer)
        let _ = rx1.await;
        println!("Round 1: Commitments sent successfully");

        // Receive commitments from all other participants
        let mut commitments_map = BTreeMap::new();
        commitments_map.insert(identifier_id, commitments);

        // Create a set of expected participant identifiers for validation
        let expected_participants: BTreeSet<Identifier<C>> = signing_participants
            .iter()
            .map(|&id| Identifier::try_from(id))
            .collect::<Result<_, _>>()
            .map_err(|e| anyhow!("invalid participant identifier: {e}"))?;

        // Create a reference set for comparison
        let expected_participants_refs: BTreeSet<_> = expected_participants.iter().collect();

        let mut received_participants = BTreeSet::new();
        received_participants.insert(identifier_id);

        while received_participants.len() < signing_participants.len() {
            println!(
                "Round 1: Waiting for commitment {}/{}",
                received_participants.len(),
                signing_participants.len() - 1
            );
            let req = rt_incoming
                .recv()
                .await
                .map_err(|e| anyhow!("error receiving message: {e}"))?;

            let sender_id = Identifier::try_from(req.from)
                .map_err(|e| anyhow!("invalid sender identifier {}: {e}", req.from))?;

            // Verify sender is in the expected participants list
            if !expected_participants.contains(&sender_id) {
                return Err(anyhow!(
                    "Received commitment from unexpected participant {} (expected: {:?})",
                    req.from,
                    signing_participants
                ));
            }

            // Skip if we already received a commitment from this participant
            if received_participants.contains(&sender_id) {
                println!(
                    "Round 1: Duplicate commitment from participant {}, skipping",
                    req.from
                );
                continue;
            }

            println!("Round 1: Received commitment from participant {}", req.from);
            let payload: Vec<u8> = serde_ipld_dagcbor::from_slice(&req.payload)
                .map_err(|e| anyhow!("failed to decode commitments: {e}"))?;
            let comms = SigningCommitments::<C>::deserialize(&payload).map_err(|e| {
                anyhow!(
                    "failed to deserialize commitments from participant {}: {e}",
                    req.from
                )
            })?;

            commitments_map.insert(sender_id, comms);
            received_participants.insert(sender_id);
        }

        // Verify we have commitments from all expected participants
        let commitments_identifiers: BTreeSet<_> = commitments_map.keys().collect();
        if commitments_identifiers != expected_participants_refs {
            return Err(anyhow!(
                "Missing commitments from some participants. Expected: {:?}, Got: {:?}",
                expected_participants,
                commitments_identifiers.iter().collect::<Vec<_>>()
            ));
        }

        // Create signing package
        println!(
            "Round 1: Collected commitments from all {} participants: {:?}",
            commitments_map.len(),
            commitments_map.keys().collect::<Vec<_>>()
        );
        let signing_package = SigningPackage::new(commitments_map, message);

        // Round 2: Generate signature share
        let signature_share = frost_core::round2::sign(&signing_package, &nonces, &key_package)
            .map_err(|e| anyhow!("failed to generate signature share: {e}"))?;

        // Send signature share
        use frost_core::round2::SignatureShare;
        let sig_share_payload = signature_share.serialize();
        let sig_share_payload_cbor = serde_ipld_dagcbor::to_vec(&sig_share_payload)
            .map_err(|e| anyhow!("failed to encode signature share: {e}"))?;
        println!(
            "Round 2: Sending signature share, waiting for {} other participants",
            signing_participants.len() - 1
        );
        let (tx2, rx2) = futures::channel::oneshot::channel();
        rt_outgoing
            .send(mpc_service::OutgoingResponse {
                body: sig_share_payload_cbor,
                to: None,
                sent_feedback: Some(tx2),
            })
            .await
            .map_err(|e| anyhow!("error sending signature share: {e}"))?;
        // Wait for message to be sent (written to buffer)
        let _ = rx2.await;
        println!("Round 2: Signature share sent successfully");

        // Receive signature shares from all other participants
        let mut signature_shares = BTreeMap::new();
        signature_shares.insert(identifier_id, signature_share);

        let mut received_signature_participants = BTreeSet::new();
        received_signature_participants.insert(identifier_id);

        while received_signature_participants.len() < signing_participants.len() {
            println!(
                "Round 2: Waiting for signature share {}/{}",
                received_signature_participants.len(),
                signing_participants.len() - 1
            );
            let req = rt_incoming
                .recv()
                .await
                .map_err(|e| anyhow!("error receiving message: {e}"))?;

            let sender_id = Identifier::try_from(req.from)
                .map_err(|e| anyhow!("invalid sender identifier {}: {e}", req.from))?;

            // Verify sender is in the expected participants list
            if !expected_participants.contains(&sender_id) {
                return Err(anyhow!(
                    "Received signature share from unexpected participant {} (expected: {:?})",
                    req.from,
                    signing_participants
                ));
            }

            // Skip if we already received a signature share from this participant
            if received_signature_participants.contains(&sender_id) {
                println!(
                    "Round 2: Duplicate signature share from participant {}, skipping",
                    req.from
                );
                continue;
            }

            println!(
                "Round 2: Received signature share from participant {}",
                req.from
            );
            let payload: Vec<u8> = serde_ipld_dagcbor::from_slice(&req.payload)
                .map_err(|e| anyhow!("failed to decode signature share: {e}"))?;
            let sig_share = SignatureShare::<C>::deserialize(&payload).map_err(|e| {
                anyhow!(
                    "failed to deserialize signature share from participant {}: {e}",
                    req.from
                )
            })?;

            signature_shares.insert(sender_id, sig_share);
            received_signature_participants.insert(sender_id);
        }

        // Verify we have signature shares from all expected participants
        let signature_shares_identifiers: BTreeSet<_> = signature_shares.keys().collect();
        if signature_shares_identifiers != expected_participants_refs {
            return Err(anyhow!(
                "Missing signature shares from some participants. Expected: {:?}, Got: {:?}",
                expected_participants,
                signature_shares_identifiers.iter().collect::<Vec<_>>()
            ));
        }

        println!(
            "Round 2: Collected signature shares from all {} participants: {:?}",
            signature_shares.len(),
            signature_shares.keys().collect::<Vec<_>>()
        );

        // Deserialize PublicKeyPackage from KeyShare
        let public_key_package = PublicKeyPackage::<C>::deserialize(&key_share.public_key_package)
            .map_err(|e| anyhow!("failed to deserialize public key package: {e}"))?;

        // Debug: Print signature share identifiers
        println!(
            "Signature shares identifiers: {:?}",
            signature_shares.keys().collect::<Vec<_>>()
        );
        println!(
            "Public key package verifying shares count: {}",
            public_key_package.verifying_shares().len()
        );
        println!(
            "Public key package verifying shares identifiers: {:?}",
            public_key_package
                .verifying_shares()
                .keys()
                .collect::<Vec<_>>()
        );

        // Verify all signature participants are in the public key package
        let public_key_identifiers: BTreeSet<_> =
            public_key_package.verifying_shares().keys().collect();
        for sig_id in signature_shares.keys() {
            if !public_key_identifiers.contains(sig_id) {
                return Err(anyhow!(
                    "Signature participant {:?} is not in the public key package. \
                    Only participants from the original keygen can sign: {:?}",
                    sig_id,
                    public_key_identifiers
                ));
            }
        }

        let signature =
            frost_core::aggregate(&signing_package, &signature_shares, &public_key_package)
                .map_err(|e| anyhow!("failed to aggregate signature: {e}"))?;

        // Verify the aggregated signature
        let group_public_key = public_key_package.verifying_key();
        group_public_key
            .verify(message, &signature)
            .map_err(|e| anyhow!("signature verification failed: {e}"))?;

        let group_public_key_bytes = group_public_key
            .serialize()
            .map_err(|e| anyhow!("failed to serialize group public key: {e}"))?;

        // Serialize signature
        let sig_bytes = signature
            .serialize()
            .map_err(|e| anyhow!("failed to serialize signature: {e}"))?;

        println!("Signing completed successfully, returning signature");
        println!("Group public key bytes: {:?}", group_public_key_bytes);
        println!("message: {:?}", message);
        println!("Signature bytes: {:?}", hex::encode(sig_bytes.as_slice()));
        Ok(Signature {
            curve: key_share.curve,
            signature: sig_bytes,
            pub_key: group_public_key_bytes,
        })
    }

    fn read_key_share(&self) -> anyhow::Result<KeyShare> {
        let share_bytes = fs::read(&self.path).context("failed to read local key")?;
        let key_share =
            serde_json::from_slice(&share_bytes).context("failed to deserialize local key")?;
        Ok(key_share)
    }
}
