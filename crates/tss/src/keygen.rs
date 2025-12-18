use crate::keysign::PublicKey;
use anyhow::{anyhow, Context};
use frost_core::{
    keys::dkg::{part1, part2, part3, round1, round2},
    Ciphersuite, Identifier,
};
use mpc_network::Curve;
use mpc_runtime::{IncomingRequest, OutgoingResponse, Peerset};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufReader, Write},
    path::Path,
};

#[derive(Serialize, Deserialize, Debug)]
pub struct KeyShare {
    pub curve: Curve,
    pub identifier: u16,
    pub signing_key: Vec<u8>,
    pub public_key: Vec<u8>,
    pub group_public_key: Vec<u8>,
    pub public_key_package: Vec<u8>, // Serialized PublicKeyPackage
    pub min_signers: u16,            // Store threshold from keygen
    pub max_signers: u16,            // Store total participants from keygen
}

pub struct KeyGen {
    path: String,
    curve: Curve,
}

#[async_trait::async_trait]
impl mpc_runtime::ComputeAgentAsync for KeyGen {
    fn protocol_id(&self) -> u64 {
        0
    }

    async fn compute(
        mut self: Box<Self>,
        mut peerset: Peerset,
        payload: Vec<u8>,
        incoming: async_channel::Receiver<IncomingRequest>,
        outgoing: async_channel::Sender<OutgoingResponse>,
    ) -> anyhow::Result<Vec<u8>> {
        println!("keygen compute called with curve: {:?}", self.curve);
        let n = peerset.len() as u16;
        let i = peerset
            .index_of(peerset.local_peer_id())
            .ok_or_else(|| anyhow!("local peer id not found in peerset"))?
            + 1;
        let mut io = BufReader::new(&*payload);
        let t = unsigned_varint::io::read_u16(&mut io)?;
        let curve = self.curve;

        let (signing_key_bytes, public_key_bytes, group_public_key_bytes, public_key_package_bytes) =
            match curve {
                Curve::Ed25519 => {
                    let (key_package, public_key_package) = self
                        .run_dkg::<frost_ed25519::Ed25519Sha512>(i, t, n, incoming, outgoing)
                        .await?;
                    let signing_share_bytes = key_package.signing_share().serialize();
                    let verifying_share_bytes = key_package
                        .verifying_share()
                        .serialize()
                        .map_err(|e| anyhow!("failed to serialize verifying share: {e}"))?;
                    let group_bytes = public_key_package
                        .verifying_key()
                        .serialize()
                        .map_err(|e| anyhow!("failed to serialize group public key: {e}"))?;
                    let pub_pkg_bytes = public_key_package
                        .serialize()
                        .map_err(|e| anyhow!("failed to serialize public key package: {e}"))?;
                    (
                        signing_share_bytes,
                        verifying_share_bytes,
                        group_bytes,
                        pub_pkg_bytes,
                    )
                }
                Curve::Secp256k1 => {
                    let (key_package, public_key_package) = self
                        .run_dkg::<frost_secp256k1::Secp256K1Sha256>(i, t, n, incoming, outgoing)
                        .await?;
                    let signing_share_bytes = key_package.signing_share().serialize();
                    let verifying_share_bytes = key_package
                        .verifying_share()
                        .serialize()
                        .map_err(|e| anyhow!("failed to serialize verifying share: {e}"))?;
                    let group_bytes = public_key_package
                        .verifying_key()
                        .serialize()
                        .map_err(|e| anyhow!("failed to serialize group public key: {e}"))?;
                    let pub_pkg_bytes = public_key_package
                        .serialize()
                        .map_err(|e| anyhow!("failed to serialize public key package: {e}"))?;
                    (
                        signing_share_bytes,
                        verifying_share_bytes,
                        group_bytes,
                        pub_pkg_bytes,
                    )
                }
            };

        let key_share_data = KeyShare {
            curve,
            identifier: i,
            signing_key: signing_key_bytes,
            public_key: public_key_bytes,
            group_public_key: group_public_key_bytes,
            public_key_package: public_key_package_bytes,
            min_signers: t,
            max_signers: n,
        };

        let group_pk_bytes = self.save_key_share(key_share_data)?;
        peerset.save_to_cache().await?;

        // Return PublicKey structure matching the expected format
        let pk = PublicKey {
            curve,
            bytes: group_pk_bytes,
        };

        let pk_bytes = serde_ipld_dagcbor::to_vec(&pk)
            .map_err(|e| anyhow!("error encoding public key {e}"))?;

        Ok(pk_bytes)
    }
}

impl KeyGen {
    pub fn new(p: &str, curve: Curve) -> Self {
        Self {
            path: p.to_owned(),
            curve,
        }
    }

    async fn run_dkg<C: Ciphersuite>(
        &self,
        identifier: u16,
        threshold: u16,
        max_signers: u16,
        incoming: async_channel::Receiver<IncomingRequest>,
        outgoing: async_channel::Sender<OutgoingResponse>,
    ) -> anyhow::Result<(
        frost_core::keys::KeyPackage<C>,
        frost_core::keys::PublicKeyPackage<C>,
    )> {
        // Convert u16 identifier to Identifier<C>
        let identifier_id =
            Identifier::try_from(identifier).map_err(|e| anyhow!("invalid identifier: {e}"))?;

        // Validate threshold parameters before calling part1
        if threshold < 2 {
            return Err(anyhow!(
                "threshold (min_signers) must be at least 2, but got {}. \
                For a {}-of-{} threshold signature, use threshold >= 2",
                threshold,
                threshold,
                max_signers
            ));
        }
        if threshold > max_signers {
            return Err(anyhow!(
                "threshold (min_signers) {} cannot be larger than max_signers (number_of_parties) {}. \
                For a {}-of-{} threshold signature, use threshold <= {}",
                threshold, max_signers, threshold, max_signers, max_signers
            ));
        }

        // Round 1: Generate commitments
        let (round1_secret_package, round1_package) = part1::<C, _>(
            identifier_id,
            max_signers,
            threshold,
            &mut rand::rngs::OsRng,
        )
        .map_err(|e| anyhow!("Round 1 part 1 error: {e}"))?;

        // Round 1: Send and receive packages
        // Send round 1 package
        let round1_payload = round1_package
            .serialize()
            .map_err(|e| anyhow!("failed to serialize round1 package: {e}"))?;
        let round1_payload_cbor = serde_ipld_dagcbor::to_vec(&round1_payload)
            .map_err(|e| anyhow!("failed to encode round1 package: {e}"))?;
        let (tx1, _rx1) = futures::channel::oneshot::channel();
        outgoing
            .send(mpc_runtime::OutgoingResponse {
                body: round1_payload_cbor,
                to: None,
                sent_feedback: Some(tx1),
            })
            .await
            .map_err(|e| anyhow!("error sending round 1 package: {e}"))?;

        // Receive round 1 packages from all other parties
        // Note: part2 expects round1_packages to contain packages from OTHER parties only (max_signers - 1)
        // We need to collect all packages including our own for part3, but part2 only needs others'
        let mut round1_packages_for_part2 = BTreeMap::new();
        let mut round1_packages_all = BTreeMap::new();
        round1_packages_all.insert(identifier_id, round1_package.clone());

        // Collect all expected identifiers
        let mut expected_ids = BTreeSet::new();
        for u in 1..=max_signers {
            if let Ok(id) = Identifier::try_from(u) {
                expected_ids.insert(id);
            }
        }

        // Receive round 1 packages until we have all expected ones
        while round1_packages_all.len() < max_signers as usize {
            let req = incoming
                .recv()
                .await
                .map_err(|e| anyhow!("error receiving message: {e}"))?;

            let payload: Vec<u8> = serde_ipld_dagcbor::from_slice(&req.payload).map_err(|e| {
                anyhow!(
                    "failed to decode round1 package (payload size: {}): {e}",
                    req.payload.len()
                )
            })?;
            let round1_pkg = round1::Package::<C>::deserialize(&payload).map_err(|e| {
                anyhow!(
                    "failed to deserialize round1 package (decoded payload size: {}): {e}",
                    payload.len()
                )
            })?;
            let sender_id = Identifier::try_from(req.from)
                .map_err(|e| anyhow!("invalid sender identifier: {e}"))?;

            // Only insert if this is an expected participant
            if expected_ids.contains(&sender_id) {
                round1_packages_all.insert(sender_id, round1_pkg.clone());
                // For part2, we only need packages from other parties (not our own)
                if sender_id != identifier_id {
                    round1_packages_for_part2.insert(sender_id, round1_pkg);
                }
            }
        }

        // Verify we have exactly max_signers packages total
        if round1_packages_all.len() != max_signers as usize {
            return Err(anyhow!(
                "Round 1: Expected {} packages total, but got {}",
                max_signers,
                round1_packages_all.len()
            ));
        }

        // Verify we have exactly max_signers - 1 packages for part2 (excluding our own)
        if round1_packages_for_part2.len() != (max_signers - 1) as usize {
            return Err(anyhow!(
                "Round 1: Expected {} packages for part2 (excluding self), but got {}",
                max_signers - 1,
                round1_packages_for_part2.len()
            ));
        }

        // Round 2: Generate shares
        // part2 expects round1_packages to contain packages from OTHER parties only (max_signers - 1)
        println!(
            "Round 1: Collected {} packages total, {} for part2 (excluding self)",
            round1_packages_all.len(),
            round1_packages_for_part2.len()
        );

        let (round2_secret_package, round2_packages_map) =
            part2(round1_secret_package, &round1_packages_for_part2)
                .map_err(|e| anyhow!("Round 2 error: {e}"))?;
        println!(
            "Round 2: Generated {} packages to send",
            round2_packages_map.len()
        );

        // Create a mapping from Identifier to u16 for recipient lookup
        let mut id_to_u16 = BTreeMap::new();
        for u in 1..=max_signers {
            if let Ok(id) = Identifier::try_from(u) {
                id_to_u16.insert(id, u);
            }
        }

        // Send round 2 packages to all other parties
        for (recipient_id, round2_package) in &round2_packages_map {
            let recipient_u16 = id_to_u16
                .get(recipient_id)
                .ok_or_else(|| anyhow!("could not find u16 for recipient identifier"))?;
            let payload = round2_package
                .serialize()
                .map_err(|e| anyhow!("failed to serialize round2 package: {e}"))?;
            let payload_cbor = serde_ipld_dagcbor::to_vec(&payload)
                .map_err(|e| anyhow!("failed to encode round2 package: {e}"))?;
            let (tx2, _rx2) = futures::channel::oneshot::channel();
            outgoing
                .send(mpc_runtime::OutgoingResponse {
                    body: payload_cbor,
                    to: Some(*recipient_u16),
                    sent_feedback: Some(tx2),
                })
                .await
                .map_err(|e| anyhow!("error sending round 2 package: {e}"))?;
        }

        // Receive round 2 packages from all other parties
        // Each party sends us one package, so we expect max_signers - 1 packages
        let mut round2_packages = BTreeMap::new();
        let mut received_senders = BTreeSet::new();

        while round2_packages.len() < (max_signers - 1) as usize {
            let req = incoming
                .recv()
                .await
                .map_err(|e| anyhow!("error receiving message: {e}"))?;
            // Only process messages sent to us (or broadcast messages)
            if req.to.is_none() || req.to == Some(identifier) {
                let sender_id = Identifier::try_from(req.from)
                    .map_err(|e| anyhow!("invalid sender identifier: {e}"))?;

                // Skip if we already received a package from this sender
                if received_senders.contains(&sender_id) {
                    continue;
                }

                let payload: Vec<u8> = serde_ipld_dagcbor::from_slice(&req.payload)
                    .map_err(|e| anyhow!("failed to decode round2 package: {e}"))?;
                let round2_pkg = round2::Package::<C>::deserialize(&payload)
                    .map_err(|e| anyhow!("failed to deserialize round2 package: {e}"))?;

                round2_packages.insert(sender_id, round2_pkg);
                received_senders.insert(sender_id);
            }
        }

        println!(
            "Round 2: Collected {} packages (expected {})",
            round2_packages.len(),
            max_signers - 1
        );

        // Round 3: Finalize DKG
        // part3 expects round1_packages to contain packages from OTHER parties only (max_signers - 1)
        // and round1_packages.len() == round2_packages.len()
        println!(
            "Round 3: Using {} round1_packages and {} round2_packages",
            round1_packages_for_part2.len(),
            round2_packages.len()
        );
        let (key_package, public_key_package) = part3(
            &round2_secret_package,
            &round1_packages_for_part2,
            &round2_packages,
        )
        .map_err(|e| anyhow!("Round 3 error: {e}"))?;

        Ok((key_package, public_key_package))
    }

    fn save_key_share(&self, key_share: KeyShare) -> anyhow::Result<Vec<u8>> {
        let path = Path::new(self.path.as_str());
        let dir = path.parent().context("failed to get parent directory")?;
        fs::create_dir_all(dir).context("failed to create directory")?;

        let mut file = fs::File::create(path).context("failed to create key share file")?;

        let share_bytes =
            serde_json::to_vec(&key_share).context("failed to serialize key share")?;

        file.write_all(&share_bytes)
            .context("failed to write local key to file")?;

        Ok(key_share.group_public_key)
    }
}
