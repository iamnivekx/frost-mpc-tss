use arrayvec::ArrayString;
use std::borrow::Cow;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]

pub struct RoomId(ArrayString<64>);

impl RoomId {
    pub fn from(id: String) -> Self {
        Self(blake3::hash(id.as_bytes()).to_hex())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn protocol_name(&self) -> Cow<'static, str> {
        Cow::Owned(format!("/room/{}", self.0.to_string()))
    }
}
