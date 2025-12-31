use anyhow::Context;
use async_trait::async_trait;
use futures::channel::oneshot;
use mpc_protocols_frost::{IncomingMessage, MessageReceiver, MessageSender, OutgoingMessage};
use mpc_service::{IncomingRequest, OutgoingResponse};

pub struct RuntimeIncoming {
    inner: async_channel::Receiver<IncomingRequest>,
}

impl RuntimeIncoming {
    pub fn new(inner: async_channel::Receiver<IncomingRequest>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl MessageReceiver for RuntimeIncoming {
    async fn recv(&mut self) -> anyhow::Result<IncomingMessage> {
        let req = self
            .inner
            .recv()
            .await
            .context("error receiving message")?;
        Ok(IncomingMessage {
            from: req.from,
            to: req.to,
            payload: req.payload,
        })
    }
}

pub struct RuntimeOutgoing {
    inner: async_channel::Sender<OutgoingResponse>,
}

impl RuntimeOutgoing {
    pub fn new(inner: async_channel::Sender<OutgoingResponse>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl MessageSender for RuntimeOutgoing {
    async fn send(&mut self, msg: OutgoingMessage) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .send(OutgoingResponse {
                body: msg.payload,
                to: msg.to,
                sent_feedback: Some(tx),
            })
            .await
            .context("error sending message")?;
        let _ = rx.await;
        Ok(())
    }
}
