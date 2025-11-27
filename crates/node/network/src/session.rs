use crate::{behaviour::Behaviour, request_responses, RoomId};
use futures::channel::mpsc;
use std::collections::hash_map::Entry;
use std::collections::HashMap;

#[derive(Debug)]
pub enum SessionError {
    Register(request_responses::RegisterError),
    AlreadyRegistered,
    ChannelClosed,
}

impl From<request_responses::RegisterError> for SessionError {
    fn from(value: request_responses::RegisterError) -> Self {
        SessionError::Register(value)
    }
}

struct SessionState {
    receiver: Option<mpsc::Receiver<request_responses::IncomingRequest>>,
}

#[derive(Default)]
pub struct SessionManager {
    sessions: HashMap<RoomId, SessionState>,
}

impl SessionManager {
    pub fn claim_or_create(
        &mut self,
        behaviour: &mut Behaviour,
        room_id: RoomId,
        max_size: usize,
    ) -> Result<mpsc::Receiver<request_responses::IncomingRequest>, SessionError> {
        match self.sessions.entry(room_id.clone()) {
            Entry::Occupied(mut entry) => entry
                .get_mut()
                .receiver
                .take()
                .ok_or(SessionError::AlreadyRegistered),
            Entry::Vacant(entry) => {
                let (tx, rx) = mpsc::channel(max_size);
                behaviour.register_room(room_id, tx)?;
                entry.insert(SessionState { receiver: Some(rx) });
                entry
                    .into_mut()
                    .receiver
                    .take()
                    .ok_or(SessionError::AlreadyRegistered)
            }
        }
    }
}
