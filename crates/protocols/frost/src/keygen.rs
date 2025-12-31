use crate::messages::{MessageReceiver, MessageSender, OutgoingMessage};
use anyhow::{anyhow, Context};
use frost_core::{
    keys::dkg::{part1, part2, part3, round1, round2},
    Ciphersuite, Identifier,
};
use mpc_protocols_codecs as codecs;
use std::collections::{BTreeMap, BTreeSet};

pub async fn run_dkg<C: Ciphersuite, R: MessageReceiver, S: MessageSender>(
    identifier: u16,
    threshold: u16,
    max_signers: u16,
    incoming: &mut R,
    outgoing: &mut S,
) -> anyhow::Result<(
    frost_core::keys::KeyPackage<C>,
    frost_core::keys::PublicKeyPackage<C>,
)> {
    let identifier_id =
        Identifier::try_from(identifier).map_err(|e| anyhow!("invalid identifier: {e}"))?;

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
            threshold,
            max_signers,
            threshold,
            max_signers,
            max_signers
        ));
    }

    let (round1_secret_package, round1_package) = part1::<C, _>(
        identifier_id,
        max_signers,
        threshold,
        &mut rand::rngs::OsRng,
    )
    .map_err(|e| anyhow!("Round 1 part 1 error: {e}"))?;

    let round1_payload = round1_package
        .serialize()
        .map_err(|e| anyhow!("failed to serialize round1 package: {e}"))?;
    let round1_payload_cbor =
        codecs::encode(&round1_payload).context("failed to encode round1 package")?;
    outgoing
        .send(OutgoingMessage {
            payload: round1_payload_cbor,
            to: None,
        })
        .await
        .context("error sending round 1 package")?;

    let mut round1_packages_for_part2 = BTreeMap::new();
    let mut round1_packages_all = BTreeMap::new();
    round1_packages_all.insert(identifier_id, round1_package.clone());

    let mut expected_ids = BTreeSet::new();
    for u in 1..=max_signers {
        if let Ok(id) = Identifier::try_from(u) {
            expected_ids.insert(id);
        }
    }

    while round1_packages_all.len() < max_signers as usize {
        let req = incoming
            .recv()
            .await
            .context("error receiving message")?;

        let payload: Vec<u8> = codecs::decode(&req.payload).with_context(|| {
            format!(
                "failed to decode round1 package (payload size: {})",
                req.payload.len()
            )
        })?;
        let round1_pkg = round1::Package::<C>::deserialize(&payload).map_err(|e| {
            anyhow!(
                "failed to deserialize round1 package (decoded payload size: {}): {e}",
                payload.len()
            )
        })?;
        let sender_id =
            Identifier::try_from(req.from).map_err(|e| anyhow!("invalid sender identifier: {e}"))?;

        if expected_ids.contains(&sender_id) {
            round1_packages_all.insert(sender_id, round1_pkg.clone());
            if sender_id != identifier_id {
                round1_packages_for_part2.insert(sender_id, round1_pkg);
            }
        }
    }

    if round1_packages_all.len() != max_signers as usize {
        return Err(anyhow!(
            "Round 1: Expected {} packages total, but got {}",
            max_signers,
            round1_packages_all.len()
        ));
    }

    if round1_packages_for_part2.len() != (max_signers - 1) as usize {
        return Err(anyhow!(
            "Round 1: Expected {} packages for part2 (excluding self), but got {}",
            max_signers - 1,
            round1_packages_for_part2.len()
        ));
    }

    let (round2_secret_package, round2_packages_map) =
        part2(round1_secret_package, &round1_packages_for_part2)
            .map_err(|e| anyhow!("Round 2 error: {e}"))?;

    let mut id_to_u16 = BTreeMap::new();
    for u in 1..=max_signers {
        if let Ok(id) = Identifier::try_from(u) {
            id_to_u16.insert(id, u);
        }
    }

    for (recipient_id, round2_package) in &round2_packages_map {
        let recipient_u16 = id_to_u16
            .get(recipient_id)
            .ok_or_else(|| anyhow!("could not find u16 for recipient identifier"))?;
        let payload = round2_package
            .serialize()
            .map_err(|e| anyhow!("failed to serialize round2 package: {e}"))?;
        let payload_cbor = codecs::encode(&payload).context("failed to encode round2 package")?;
        outgoing
            .send(OutgoingMessage {
                payload: payload_cbor,
                to: Some(*recipient_u16),
            })
            .await
            .context("error sending round 2 package")?;
    }

    let mut round2_packages = BTreeMap::new();
    let mut received_senders = BTreeSet::new();

    while round2_packages.len() < (max_signers - 1) as usize {
        let req = incoming
            .recv()
            .await
            .context("error receiving message")?;
        if req.to.is_none() || req.to == Some(identifier) {
            let sender_id = Identifier::try_from(req.from)
                .map_err(|e| anyhow!("invalid sender identifier: {e}"))?;

            if received_senders.contains(&sender_id) {
                continue;
            }

            let payload: Vec<u8> =
                codecs::decode(&req.payload).context("failed to decode round2 package")?;
            let round2_pkg = round2::Package::<C>::deserialize(&payload)
                .map_err(|e| anyhow!("failed to deserialize round2 package: {e}"))?;

            round2_packages.insert(sender_id, round2_pkg);
            received_senders.insert(sender_id);
        }
    }

    let (key_package, public_key_package) = part3(
        &round2_secret_package,
        &round1_packages_for_part2,
        &round2_packages,
    )
    .map_err(|e| anyhow!("Round 3 error: {e}"))?;

    Ok((key_package, public_key_package))
}
