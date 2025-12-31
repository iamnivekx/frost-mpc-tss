use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub from: u16,
    pub to: Option<u16>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct OutgoingMessage {
    pub to: Option<u16>,
    pub payload: Vec<u8>,
}

#[async_trait]
pub trait MessageReceiver {
    async fn recv(&mut self) -> anyhow::Result<IncomingMessage>;
}

#[async_trait]
pub trait MessageSender {
    async fn send(&mut self, msg: OutgoingMessage) -> anyhow::Result<()>;
}
