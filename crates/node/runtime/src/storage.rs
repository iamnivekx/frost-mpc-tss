use crate::peerset::Peerset;
use crate::PeersetStorage;
use anyhow::anyhow;
use libp2p::PeerId;
use mpc_network::RoomId;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Ephemeral {
    storage: HashMap<RoomId, Peerset>,
}

impl PeersetStorage for Ephemeral {
    fn read_peerset(&self, room_id: &RoomId) -> anyhow::Result<Peerset> {
        match self.storage.get(room_id) {
            Some(p) => Ok(p.clone()),
            None => Err(anyhow!("no cache exists for room")),
        }
    }

    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> anyhow::Result<()> {
        self.storage
            .entry(room_id.clone())
            .and_modify(|e| *e = peerset.clone())
            .or_insert(peerset);
        Ok(())
    }
}

#[derive(Clone)]
pub struct LocalStorage {
    local_peer_id: PeerId,
    path: PathBuf,
}

impl LocalStorage {
    pub fn new<P: AsRef<Path>>(p: P, local_peer_id: PeerId) -> Self {
        Self {
            path: PathBuf::from(p.as_ref()),
            local_peer_id,
        }
    }
}

impl PeersetStorage for LocalStorage {
    fn read_peerset(&self, room_id: &RoomId) -> anyhow::Result<Peerset> {
        let buf = fs::read(self.path.join(room_id.as_str()))
            .map_err(|e| anyhow!("error reading peerset cache file: {e}"))?;

        let (peerset, _) = Peerset::from_bytes(&*buf, self.local_peer_id)
            .map_err(|e| anyhow!("error recover peerset : {e}"))?;

        Ok(peerset)
    }

    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> anyhow::Result<()> {
        let path = self.path.join(room_id.as_str());
        let dir = path.parent().unwrap();
        let peerset_bytes = peerset.to_bytes().unwrap();
        fs::create_dir_all(dir).unwrap();
        fs::write(path, peerset_bytes).map_err(|e| anyhow!("error writing to file: {e}"))?;

        Ok(())
    }
}
