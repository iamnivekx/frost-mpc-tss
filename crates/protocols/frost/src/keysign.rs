use crate::messages::{MessageReceiver, MessageSender, OutgoingMessage};
use crate::types::{KeyShare, Signature};
use anyhow::{anyhow, Context};
use frost_core::{
    keys::{KeyPackage, PublicKeyPackage, SigningShare, VerifyingShare},
    Ciphersuite, Identifier, SigningPackage,
};
use mpc_protocols_codecs as codecs;
use std::collections::{BTreeMap, BTreeSet};

pub async fn run_signing<C: Ciphersuite, R: MessageReceiver, S: MessageSender>(
    key_share: &KeyShare,
    _identifier: u16,
    signing_participants: &[u16],
    message: &[u8],
    incoming: &mut R,
    outgoing: &mut S,
) -> anyhow::Result<Signature> {
    let key_share_identifier = key_share.identifier;

    let identifier_id = Identifier::try_from(key_share_identifier)
        .map_err(|e| anyhow!("invalid identifier: {e}"))?;

    let signing_share = SigningShare::<C>::deserialize(&key_share.signing_key)
        .map_err(|e| anyhow!("failed to deserialize signing share: {e}"))?;
    let verifying_share = VerifyingShare::<C>::deserialize(&key_share.public_key)
        .map_err(|e| anyhow!("failed to deserialize verifying share: {e}"))?;
    let verifying_key = frost_core::VerifyingKey::<C>::deserialize(&key_share.group_public_key)
        .map_err(|e| anyhow!("failed to deserialize group public key: {e}"))?;

    let min_signers = key_share.min_signers;
    let key_package = KeyPackage::new(
        identifier_id,
        signing_share,
        verifying_share,
        verifying_key,
        min_signers,
    );

    let (nonces, commitments) =
        frost_core::round1::commit(key_package.signing_share(), &mut rand::rngs::OsRng);

    use frost_core::round1::SigningCommitments;
    let commitments_payload = commitments
        .serialize()
        .map_err(|e| anyhow!("failed to serialize commitments: {e}"))?;
    let commitments_payload_cbor =
        codecs::encode(&commitments_payload).context("failed to encode commitments")?;
    outgoing
        .send(OutgoingMessage {
            payload: commitments_payload_cbor,
            to: None,
        })
        .await
        .context("error sending commitments")?;

    let mut commitments_map = BTreeMap::new();
    commitments_map.insert(identifier_id, commitments);

    let expected_participants: BTreeSet<Identifier<C>> = signing_participants
        .iter()
        .map(|&id| Identifier::try_from(id))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow!("invalid participant identifier: {e}"))?;

    let expected_participants_refs: BTreeSet<_> = expected_participants.iter().collect();

    let mut received_participants = BTreeSet::new();
    received_participants.insert(identifier_id);

    while received_participants.len() < signing_participants.len() {
        let req = incoming
            .recv()
            .await
            .context("error receiving message")?;

        let sender_id = Identifier::try_from(req.from)
            .map_err(|e| anyhow!("invalid sender identifier {}: {e}", req.from))?;

        if !expected_participants.contains(&sender_id) {
            return Err(anyhow!(
                "Received commitment from unexpected participant {} (expected: {:?})",
                req.from,
                signing_participants
            ));
        }

        if received_participants.contains(&sender_id) {
            continue;
        }

        let payload: Vec<u8> =
            codecs::decode(&req.payload).context("failed to decode commitments")?;
        let comms = SigningCommitments::<C>::deserialize(&payload).map_err(|e| {
            anyhow!(
                "failed to deserialize commitments from participant {}: {e}",
                req.from
            )
        })?;

        commitments_map.insert(sender_id, comms);
        received_participants.insert(sender_id);
    }

    let commitments_identifiers: BTreeSet<_> = commitments_map.keys().collect();
    if commitments_identifiers != expected_participants_refs {
        return Err(anyhow!(
            "Missing commitments from some participants. Expected: {:?}, Got: {:?}",
            expected_participants,
            commitments_identifiers.iter().collect::<Vec<_>>()
        ));
    }

    let signing_package = SigningPackage::new(commitments_map, message);

    let signature_share = frost_core::round2::sign(&signing_package, &nonces, &key_package)
        .map_err(|e| anyhow!("failed to generate signature share: {e}"))?;

    use frost_core::round2::SignatureShare;
    let sig_share_payload = signature_share.serialize();
    let sig_share_payload_cbor =
        codecs::encode(&sig_share_payload).context("failed to encode signature share")?;
    outgoing
        .send(OutgoingMessage {
            payload: sig_share_payload_cbor,
            to: None,
        })
        .await
        .context("error sending signature share")?;

    let mut signature_shares = BTreeMap::new();
    signature_shares.insert(identifier_id, signature_share);

    let mut received_signature_participants = BTreeSet::new();
    received_signature_participants.insert(identifier_id);

    while received_signature_participants.len() < signing_participants.len() {
        let req = incoming
            .recv()
            .await
            .context("error receiving message")?;

        let sender_id = Identifier::try_from(req.from)
            .map_err(|e| anyhow!("invalid sender identifier {}: {e}", req.from))?;

        if !expected_participants.contains(&sender_id) {
            return Err(anyhow!(
                "Received signature share from unexpected participant {} (expected: {:?})",
                req.from,
                signing_participants
            ));
        }

        if received_signature_participants.contains(&sender_id) {
            continue;
        }

        let payload: Vec<u8> =
            codecs::decode(&req.payload).context("failed to decode signature share")?;
        let sig_share = SignatureShare::<C>::deserialize(&payload).map_err(|e| {
            anyhow!(
                "failed to deserialize signature share from participant {}: {e}",
                req.from
            )
        })?;

        signature_shares.insert(sender_id, sig_share);
        received_signature_participants.insert(sender_id);
    }

    let signature_shares_identifiers: BTreeSet<_> = signature_shares.keys().collect();
    if signature_shares_identifiers != expected_participants_refs {
        return Err(anyhow!(
            "Missing signature shares from some participants. Expected: {:?}, Got: {:?}",
            expected_participants,
            signature_shares_identifiers.iter().collect::<Vec<_>>()
        ));
    }

    let public_key_package = PublicKeyPackage::<C>::deserialize(&key_share.public_key_package)
        .map_err(|e| anyhow!("failed to deserialize public key package: {e}"))?;

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

    let group_public_key = public_key_package.verifying_key();
    group_public_key
        .verify(message, &signature)
        .map_err(|e| anyhow!("signature verification failed: {e}"))?;

    let group_public_key_bytes = group_public_key
        .serialize()
        .map_err(|e| anyhow!("failed to serialize group public key: {e}"))?;

    let sig_bytes = signature
        .serialize()
        .map_err(|e| anyhow!("failed to serialize signature: {e}"))?;

    Ok(Signature {
        curve: key_share.curve,
        signature: sig_bytes,
        pub_key: group_public_key_bytes,
    })
}
