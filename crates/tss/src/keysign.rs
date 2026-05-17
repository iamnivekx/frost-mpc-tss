use crate::{decode_sign_payload, keygen::KeyShare, wallet_key_share_path};
use anyhow::{anyhow, Context};
use frost_core::{
    keys::{KeyPackage, PublicKeyPackage, SigningShare, VerifyingShare},
    Ciphersuite, Identifier, SigningPackage,
};
use libp2p::PeerId;
use mpc_network::Curve;
use mpc_service::{IncomingRequest, OutgoingResponse, Peerset};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    str::FromStr,
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

pub(crate) fn apply_key_share_participants(
    parties: &mut Peerset,
    key_share: &KeyShare,
) -> anyhow::Result<u16> {
    let (mapped_parties, local_identifier) =
        key_share_parties_for_peers(parties.peers(), parties.local_peer_id(), key_share)?;
    parties.parties = mapped_parties;

    Ok(local_identifier)
}

pub(crate) fn key_share_parties_for_peers(
    peers: Vec<PeerId>,
    local_peer_id: &PeerId,
    key_share: &KeyShare,
) -> anyhow::Result<(Vec<usize>, u16)> {
    if key_share.participants.is_empty() {
        return Err(anyhow!("key share does not contain participant mapping"));
    }

    let mut identifiers = HashMap::new();
    let mut used_identifiers = HashSet::new();
    for participant in &key_share.participants {
        let peer_id = PeerId::from_str(&participant.peer_id)
            .map_err(|e| anyhow!("invalid participant peer id {}: {e}", participant.peer_id))?;
        if participant.identifier == 0 {
            return Err(anyhow!("participant identifier must be one-based"));
        }
        if participant.identifier > key_share.max_signers {
            return Err(anyhow!(
                "participant identifier {} exceeds max signers {}",
                participant.identifier,
                key_share.max_signers
            ));
        }
        if !used_identifiers.insert(participant.identifier) {
            return Err(anyhow!(
                "duplicate participant identifier {}",
                participant.identifier
            ));
        }
        if identifiers
            .insert(peer_id, participant.identifier)
            .is_some()
        {
            return Err(anyhow!("duplicate participant peer id"));
        }
    }

    let mut mapped_parties = Vec::with_capacity(peers.len());
    for peer_id in peers {
        let identifier = identifiers
            .get(&peer_id)
            .ok_or_else(|| anyhow!("peer {} is not in wallet key share", peer_id.to_base58()))?;
        mapped_parties.push((*identifier - 1) as usize);
    }

    let local_identifier = *identifiers
        .get(local_peer_id)
        .ok_or_else(|| anyhow!("local peer is not in wallet key share"))?;

    Ok((mapped_parties, local_identifier))
}

fn participant_identifier_for_session_index(
    session_index: u16,
    signing_participants: &[u16],
) -> anyhow::Result<u16> {
    let position = session_index
        .checked_sub(1)
        .ok_or_else(|| anyhow!("participant index must be one-based"))? as usize;
    signing_participants.get(position).copied().ok_or_else(|| {
        anyhow!(
            "participant index {} is outside signing session",
            session_index
        )
    })
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
        let request = decode_sign_payload(&payload)?;
        let key_share = self.read_key_share(&request.wallet_id)?;
        let i = apply_key_share_participants(&mut parties, &key_share)?;
        let signing_participants: Vec<u16> = parties
            .parties
            .iter()
            .map(|idx| (*idx + 1) as u16)
            .collect();
        let message = request.message;

        println!("Signing participants: {:?}", signing_participants);
        println!("Current identifier: {}", i);
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
                    &message,
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
                    &message,
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
        use frost_core::round2::SignatureShare;
        let mut deferred_signature_shares = BTreeMap::new();

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

            let sender_identifier =
                participant_identifier_for_session_index(req.from, signing_participants)?;
            let sender_id = Identifier::try_from(sender_identifier)
                .map_err(|e| anyhow!("invalid sender identifier {}: {e}", sender_identifier))?;

            // Verify sender is in the expected participants list
            if !expected_participants.contains(&sender_id) {
                return Err(anyhow!(
                    "Received commitment from unexpected participant {} (expected: {:?})",
                    sender_identifier,
                    signing_participants
                ));
            }
            let payload: Vec<u8> = serde_ipld_dagcbor::from_slice(&req.payload)
                .map_err(|e| anyhow!("failed to decode commitments: {e}"))?;

            // If commitment from this sender is already collected, this can be an early round2 share.
            if received_participants.contains(&sender_id) {
                if let Ok(sig_share) = SignatureShare::<C>::deserialize(&payload) {
                    deferred_signature_shares
                        .entry(sender_id)
                        .or_insert(sig_share);
                    println!(
                        "Round 1: Buffered early signature share from participant {}",
                        sender_identifier
                    );
                } else {
                    println!(
                        "Round 1: Duplicate commitment from participant {}, skipping",
                        sender_identifier
                    );
                }
                continue;
            }

            println!(
                "Round 1: Received commitment from participant {}",
                sender_identifier
            );
            match SigningCommitments::<C>::deserialize(&payload) {
                Ok(comms) => {
                    commitments_map.insert(sender_id, comms);
                    received_participants.insert(sender_id);
                }
                Err(commit_err) => {
                    // A round2 share can arrive early on asynchronous networks.
                    if let Ok(sig_share) = SignatureShare::<C>::deserialize(&payload) {
                        deferred_signature_shares
                            .entry(sender_id)
                            .or_insert(sig_share);
                        println!(
                            "Round 1: Buffered early signature share from participant {}",
                            sender_identifier
                        );
                        continue;
                    }
                    return Err(anyhow!(
                        "failed to deserialize commitments from participant {}: {}",
                        sender_identifier,
                        commit_err
                    ));
                }
            }
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
        for (sender_id, sig_share) in deferred_signature_shares {
            if expected_participants.contains(&sender_id)
                && !received_signature_participants.contains(&sender_id)
            {
                signature_shares.insert(sender_id, sig_share);
                received_signature_participants.insert(sender_id);
            }
        }

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

            let sender_identifier =
                participant_identifier_for_session_index(req.from, signing_participants)?;
            let sender_id = Identifier::try_from(sender_identifier)
                .map_err(|e| anyhow!("invalid sender identifier {}: {e}", sender_identifier))?;

            // Verify sender is in the expected participants list
            if !expected_participants.contains(&sender_id) {
                return Err(anyhow!(
                    "Received signature share from unexpected participant {} (expected: {:?})",
                    sender_identifier,
                    signing_participants
                ));
            }

            // Skip if we already received a signature share from this participant
            if received_signature_participants.contains(&sender_id) {
                println!(
                    "Round 2: Duplicate signature share from participant {}, skipping",
                    sender_identifier
                );
                continue;
            }

            println!(
                "Round 2: Received signature share from participant {}",
                sender_identifier
            );
            let payload: Vec<u8> = serde_ipld_dagcbor::from_slice(&req.payload)
                .map_err(|e| anyhow!("failed to decode signature share: {e}"))?;
            let sig_share = match SignatureShare::<C>::deserialize(&payload) {
                Ok(sig_share) => sig_share,
                Err(e) => {
                    // Late round1 commitment can still be in flight; ignore it.
                    if SigningCommitments::<C>::deserialize(&payload).is_ok() {
                        println!(
                            "Round 2: Ignoring late commitment payload from participant {}",
                            sender_identifier
                        );
                        continue;
                    }
                    return Err(anyhow!(
                        "failed to deserialize signature share from participant {}: {e}",
                        sender_identifier
                    ));
                }
            };

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

    fn read_key_share(&self, wallet_id: &str) -> anyhow::Result<KeyShare> {
        let share_path = wallet_key_share_path(self.path.as_str(), wallet_id);
        let share_bytes = fs::read(&share_path).context("failed to read local key")?;
        let key_share =
            serde_json::from_slice(&share_bytes).context("failed to deserialize local key")?;
        Ok(key_share)
    }
}

#[cfg(test)]
mod tests {
    use super::{key_share_parties_for_peers, participant_identifier_for_session_index};
    use crate::keygen::{KeyShare, KeyShareParticipant};
    use libp2p::PeerId;
    use mpc_network::Curve;
    use std::hint::black_box;
    use std::time::Instant;

    #[test]
    fn sign_session_recovers_original_identifiers_from_key_share() {
        let remote_peer = PeerId::random();
        let skipped_peer = PeerId::random();
        let local_peer = PeerId::random();
        let key_share = KeyShare {
            curve: Curve::Ed25519,
            identifier: 3,
            signing_key: vec![1],
            public_key: vec![2],
            group_public_key: vec![3],
            public_key_package: vec![4],
            min_signers: 2,
            max_signers: 3,
            participants: vec![
                KeyShareParticipant {
                    peer_id: remote_peer.to_base58(),
                    identifier: 1,
                },
                KeyShareParticipant {
                    peer_id: skipped_peer.to_base58(),
                    identifier: 2,
                },
                KeyShareParticipant {
                    peer_id: local_peer.to_base58(),
                    identifier: 3,
                },
            ],
        };

        let (parties, local_identifier) =
            key_share_parties_for_peers(vec![remote_peer, local_peer], &local_peer, &key_share)
                .unwrap();

        assert_eq!(local_identifier, 3);
        assert_eq!(parties, vec![0, 2]);
    }

    #[test]
    fn sign_session_rejects_duplicate_participant_identifiers() {
        let remote_peer = PeerId::random();
        let local_peer = PeerId::random();
        let key_share = KeyShare {
            curve: Curve::Ed25519,
            identifier: 1,
            signing_key: vec![1],
            public_key: vec![2],
            group_public_key: vec![3],
            public_key_package: vec![4],
            min_signers: 2,
            max_signers: 2,
            participants: vec![
                KeyShareParticipant {
                    peer_id: remote_peer.to_base58(),
                    identifier: 1,
                },
                KeyShareParticipant {
                    peer_id: local_peer.to_base58(),
                    identifier: 1,
                },
            ],
        };

        let err =
            key_share_parties_for_peers(vec![remote_peer, local_peer], &local_peer, &key_share)
                .unwrap_err();

        assert!(err.to_string().contains("duplicate participant identifier"));
    }

    #[test]
    fn sign_session_rejects_participant_identifier_outside_key_share_size() {
        let remote_peer = PeerId::random();
        let local_peer = PeerId::random();
        let key_share = KeyShare {
            curve: Curve::Ed25519,
            identifier: 1,
            signing_key: vec![1],
            public_key: vec![2],
            group_public_key: vec![3],
            public_key_package: vec![4],
            min_signers: 2,
            max_signers: 2,
            participants: vec![
                KeyShareParticipant {
                    peer_id: remote_peer.to_base58(),
                    identifier: 1,
                },
                KeyShareParticipant {
                    peer_id: local_peer.to_base58(),
                    identifier: 3,
                },
            ],
        };

        let err =
            key_share_parties_for_peers(vec![remote_peer, local_peer], &local_peer, &key_share)
                .unwrap_err();

        assert!(err
            .to_string()
            .contains("participant identifier 3 exceeds max signers 2"));
    }

    #[test]
    fn session_index_maps_to_key_share_identifier_order() {
        let signing_participants = vec![1, 3];

        assert_eq!(
            participant_identifier_for_session_index(1, &signing_participants).unwrap(),
            1
        );
        assert_eq!(
            participant_identifier_for_session_index(2, &signing_participants).unwrap(),
            3
        );
    }

    #[test]
    #[ignore = "benchmark"]
    fn benchmark_key_share_participant_mapping() {
        let peers = (0..256).map(|_| PeerId::random()).collect::<Vec<_>>();
        let local_peer = peers[128];
        let key_share = KeyShare {
            curve: Curve::Ed25519,
            identifier: 129,
            signing_key: vec![1],
            public_key: vec![2],
            group_public_key: vec![3],
            public_key_package: vec![4],
            min_signers: 128,
            max_signers: 256,
            participants: peers
                .iter()
                .enumerate()
                .map(|(index, peer_id)| KeyShareParticipant {
                    peer_id: peer_id.to_base58(),
                    identifier: index as u16 + 1,
                })
                .collect(),
        };

        let iterations = 1_000;
        let started = Instant::now();
        for _ in 0..iterations {
            let (parties, local_identifier) =
                key_share_parties_for_peers(peers.clone(), &local_peer, &key_share).unwrap();
            black_box((parties, local_identifier));
        }
        let elapsed = started.elapsed();
        println!(
            "mapped {} participants for {} iterations in {:?} ({:?}/iteration)",
            peers.len(),
            iterations,
            elapsed,
            elapsed / iterations
        );
    }
}
