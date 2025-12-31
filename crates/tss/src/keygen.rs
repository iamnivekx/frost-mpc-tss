use crate::adapter::{RuntimeIncoming, RuntimeOutgoing};
use anyhow::{anyhow, Context};
use mpc_network::Curve;
use mpc_protocols_codecs as codecs;
use mpc_protocols_frost::{run_dkg, KeyShare, PublicKey};
use mpc_runtime::{IncomingRequest, OutgoingResponse, Peerset};
use std::{
    fs,
    io::{BufReader, Write},
    path::Path,
};

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

        let mut protocol_incoming = RuntimeIncoming::new(incoming);
        let mut protocol_outgoing = RuntimeOutgoing::new(outgoing);

        let (signing_key_bytes, public_key_bytes, group_public_key_bytes, public_key_package_bytes) =
            match curve {
                Curve::Ed25519 => {
                    let (key_package, public_key_package) =
                        run_dkg::<frost_ed25519::Ed25519Sha512, _, _>(
                            i,
                            t,
                            n,
                            &mut protocol_incoming,
                            &mut protocol_outgoing,
                        )
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
                    let (key_package, public_key_package) =
                        run_dkg::<frost_secp256k1::Secp256K1Sha256, _, _>(
                            i,
                            t,
                            n,
                            &mut protocol_incoming,
                            &mut protocol_outgoing,
                        )
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

        let pk = PublicKey {
            curve,
            bytes: group_pk_bytes,
        };

        let pk_bytes =
            codecs::encode(&pk).map_err(|e| anyhow!("error encoding public key {e}"))?;

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
