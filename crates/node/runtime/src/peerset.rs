use futures::channel::{mpsc, oneshot};
use futures_util::SinkExt;
use itertools::Itertools;
use libp2p::PeerId;
use std::ops::Index;
use std::{
    io,
    io::{BufReader, BufWriter, Read, Write},
};
use tracing::{debug, info, warn};

#[derive(Clone)]
pub struct Peerset {
    local_peer_id: PeerId,
    peers: Vec<PeerId>,
    pub parties: Vec<usize>,
    tx: mpsc::Sender<PeersetMsg>,
}

pub(crate) enum PeersetMsg {
    ReadFromCache(oneshot::Sender<anyhow::Result<Peerset>>),
    WriteToCache(Peerset, oneshot::Sender<anyhow::Result<()>>),
}

impl Peerset {
    pub(crate) fn new(
        peers: impl Iterator<Item = PeerId>,
        local_peer_id: PeerId,
    ) -> (Self, mpsc::Receiver<PeersetMsg>) {
        let (tx, rx) = mpsc::channel(1);
        let peers: Vec<_> = peers.sorted_by_key(|p| p.to_bytes()).collect();
        (
            Self {
                local_peer_id,
                parties: (0..peers.len()).collect(),
                peers,
                tx,
            },
            rx,
        )
    }

    pub(crate) fn from_bytes(
        bytes: &[u8],
        local_peer_id: PeerId,
    ) -> io::Result<(Self, mpsc::Receiver<PeersetMsg>)> {
        let mut io = BufReader::new(bytes);

        // Read the local peer id
        let local_peer_id_len = unsigned_varint::io::read_usize(&mut io)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        let mut local_peer_id_buffer = vec![0; local_peer_id_len];
        io.read_exact(&mut local_peer_id_buffer)?;

        let recover_local_peer_id = PeerId::from_bytes(&local_peer_id_buffer)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
        debug!("recover_local_peer_id : {:?}", recover_local_peer_id);
        debug!("local_peer_id : {:?}", local_peer_id.clone());
        let mut nodes = vec![];
        // self.nodes
        {
            // Read the nodes len
            let nodes_len = unsigned_varint::io::read_usize(&mut io)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

            for _ in 0..nodes_len {
                // Read the peer id len
                let peer_id_len = unsigned_varint::io::read_usize(&mut io)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

                let mut peer_id_buffer = vec![0; peer_id_len];
                io.read_exact(&mut peer_id_buffer)?;

                let peer_id = PeerId::from_bytes(&peer_id_buffer)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

                nodes.push(peer_id);
            }
        }
        let mut parties = vec![];
        {
            // Read the parties len
            let parties_len = unsigned_varint::io::read_usize(&mut io)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

            for _ in 0..parties_len {
                // Read the peer id len
                let parties_index = unsigned_varint::io::read_usize(&mut io)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

                parties.push(parties_index);
            }
        }

        let (tx, rx) = mpsc::channel(1);
        Ok((
            Self {
                local_peer_id,
                peers: nodes,
                parties,
                tx,
            },
            rx,
        ))
    }

    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        let b = vec![];
        let mut io = BufWriter::new(b);
        // Write the local peer id, size + data
        {
            let local_peer_id_bytes = self.local_peer_id.to_bytes();
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                local_peer_id_bytes.len(),
                &mut buffer,
            ))?;
            io.write_all(&*local_peer_id_bytes).unwrap();
        }
        // Write the nodes. size([size + data,...])
        {
            let node_len = self.peers.len();
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(node_len, &mut buffer))?;

            for (_i, peer_id) in self.peers.clone().into_iter().enumerate() {
                let peer_id_bytes = peer_id.to_bytes();
                let mut buffer = unsigned_varint::encode::usize_buffer();
                io.write_all(unsigned_varint::encode::usize(
                    peer_id_bytes.len(),
                    &mut buffer,
                ))?;
                io.write_all(&*peer_id_bytes)?;
            }
        }
        // parties
        {
            let parties_len = self.parties.len();
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(parties_len, &mut buffer))?;

            for (_, index) in self.parties.clone().into_iter().enumerate() {
                let mut buffer = unsigned_varint::encode::usize_buffer();
                io.write_all(unsigned_varint::encode::usize(index, &mut buffer))?;
            }
        }

        Ok(io.buffer().to_vec())
    }

    pub async fn recover_from_cache(&mut self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(PeersetMsg::ReadFromCache(tx)).await;
        let cache = rx.await.expect("runtime expected to serve protocol")?;
        let mut parties = vec![];
        for peer_id in self.peers.iter().sorted_by_key(|p| p.to_bytes()) {
            match cache.index_of(peer_id) {
                Some(i) => {
                    parties.push(cache.parties[i as usize]);
                }
                None => {
                    warn!(
                        "Peer {} does not appear in the peerset cache, skipping.",
                        peer_id.to_base58()
                    )
                }
            }
        }

        self.parties = parties;
        Ok(())
    }

    pub async fn save_to_cache(&mut self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .tx
            .send(PeersetMsg::WriteToCache(self.clone(), tx))
            .await;
        rx.await.expect("runtime expected to serve protocol")
    }

    pub fn index_of(&self, peer_id: &PeerId) -> Option<u16> {
        self.peers
            .iter()
            .position(|elem| *elem == *peer_id)
            .map(|i| i as u16)
    }

    pub fn size(&self) -> usize {
        self.peers.len()
    }

    pub fn remotes_iter(self) -> impl Iterator<Item = PeerId> {
        self.peers
            .into_iter()
            .enumerate()
            .filter(move |(_i, p)| *p != self.local_peer_id)
            .map(|(_i, p)| p.clone())
    }

    pub fn local_peer_id(&self) -> &PeerId {
        return &self.local_peer_id;
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn peers(&self) -> Vec<PeerId> {
        return self.peers.to_vec().clone();
    }
}

impl Index<u16> for Peerset {
    type Output = PeerId;

    fn index(&self, index: u16) -> &Self::Output {
        &self.peers[index as usize]
    }
}

impl IntoIterator for Peerset {
    type Item = PeerId;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.peers.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use crate::peerset::Peerset;
    use libp2p::PeerId;
    use std::str::FromStr;

    #[test]
    fn peerset_encoding() {
        let peer_ids = vec![
            PeerId::from_str("12D3KooWMQmcJA5raTtuxqAguM5CiXRhEDumLNmZQ7PmKZizjFBX").unwrap(),
            PeerId::from_str("12D3KooWHYG3YsVs9hTwbgPKVrTrPQBKc8FnDhV6bsJ4W37eds8p").unwrap(),
        ];
        let local_peer_id = peer_ids[0];
        let (mut peerset, _) = Peerset::new(peer_ids.into_iter(), local_peer_id);
        peerset.parties = vec![0, 2];
        let encoded = peerset.to_bytes().unwrap();
        let (decoded, _) = Peerset::from_bytes(&*encoded, local_peer_id).unwrap();

        println!("original: {:?}, {:?}", peerset.parties, peerset.peers);
        println!("decoded: {:?}, {:?}", decoded.parties, decoded.peers);

        assert_eq!(peerset.parties, decoded.parties);
    }
}
