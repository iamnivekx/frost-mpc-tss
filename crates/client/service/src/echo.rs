use blake2::{Blake2s256, Digest};
use futures::channel::{mpsc, oneshot};
use futures_util::{pin_mut, select, FutureExt, StreamExt};
use libp2p::PeerId;
use mpc_network::request_responses;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashSet};
use std::future::Future;
use std::io::Write;

pub(crate) struct EchoGadget {
    r: u16,
    n: usize,
    msgs: BTreeMap<u64, BinaryHeap<EchoMessage>>,
    rx: mpsc::Receiver<EchoMessage>,
}

impl EchoGadget {
    pub fn new(n: usize) -> (Self, mpsc::Sender<EchoMessage>) {
        let (tx, rx) = mpsc::channel(n);

        let gadget = EchoGadget {
            r: 0,
            n,
            msgs: Default::default(),
            rx,
        };

        (gadget, tx)
    }

    pub async fn wrap_execution(
        mut self,
        computation_fut: impl Future<Output = crate::Result<()>> + Unpin,
    ) -> crate::Result<()> {
        let mut echo = Box::pin(self.proceed_round().fuse());
        let future = computation_fut.fuse();
        pin_mut!(future);
        let mut computation_done = false;

        loop {
            if computation_done {
                match echo.await {
                    Ok(Some(s)) => {
                        echo = Box::pin(s.proceed_round().fuse());
                        continue;
                    }
                    Ok(None) => return Ok(()),
                    Err(e) => return Err(e),
                }
            }

            select! {
                echo_res = echo => match echo_res {
                    Ok(Some(s)) => {
                        echo = Box::pin(s.proceed_round().fuse());
                    },
                    Ok(None) => {
                        return future.await;
                    },
                    Err(e) => {
                        // Echo failed - return the error
                        // ProtocolExecution will handle sending the error response
                        return Err(e);
                    }
                },
                comp_res = future => {
                    comp_res?;
                    computation_done = true;
                }
            }
        }
    }

    async fn proceed_round(&mut self) -> crate::Result<Option<&mut Self>> {
        let message_id = loop {
            if let Some(message_id) = self.ready_message_id() {
                break message_id;
            }

            match self.rx.next().await {
                Some(msg) => {
                    self.msgs.entry(msg.message_id).or_default().push(msg);
                }
                None if self.msgs.is_empty() => return Ok(None),
                None => {
                    return Err(crate::Error::InternalError(anyhow::anyhow!(
                        "echo channel closed with pending messages"
                    )));
                }
            }
        };

        let mut group = self
            .msgs
            .remove(&message_id)
            .expect("ready message group must exist");

        loop {
            if group.len() == self.n {
                break;
            }

            match self.rx.next().await {
                Some(msg) if msg.message_id == message_id => {
                    group.push(msg);
                }
                Some(msg) => {
                    self.msgs.entry(msg.message_id).or_default().push(msg);
                }
                None => {
                    return Err(crate::Error::InternalError(anyhow::anyhow!(
                        "echo channel closed before message {message_id} completed"
                    )));
                }
            }
        }

        // Sort messages by sender to ensure consistent ordering
        let mut msgs_vec: Vec<_> = group.drain().collect();
        msgs_vec.sort_by_key(|m| m.sender);

        println!(
            "Echo: Collected {} messages for message {} from senders: {:?}",
            msgs_vec.len(),
            message_id,
            msgs_vec.iter().map(|m| m.sender).collect::<Vec<_>>()
        );

        let mut hasher = Blake2s256::new();
        let mut incoming_acks = vec![];
        let mut outgoing_resp_rx = None;
        let mut senders = HashSet::new();

        for echo_msg in msgs_vec {
            if !senders.insert(echo_msg.sender) {
                println!(
                    "Echo: Duplicate sender {} for message {}",
                    echo_msg.sender, message_id
                );
                return Err(crate::Error::InconsistentEcho(echo_msg.sender));
            }
            let _ = hasher.write(&*echo_msg.payload);
            match echo_msg.response {
                EchoResponse::Incoming(tx) => incoming_acks.push(tx),
                EchoResponse::Outgoing(resp_rx) => {
                    let _ = outgoing_resp_rx.insert(resp_rx);
                }
            }
        }

        let mut outgoing_resp_rx = outgoing_resp_rx.expect("outgoing message was expected");

        let echo_hash = hasher.finalize().to_vec();
        for tx in incoming_acks.into_iter() {
            tx.send(request_responses::OutgoingResponse {
                result: Ok(echo_hash.clone()),
                sent_feedback: None,
            })
            .expect("expected to be able to send acknowledgment with echoing module");
        }

        let mut echo_hashes = vec![];

        loop {
            echo_hashes.push(outgoing_resp_rx.select_next_some().await);

            if echo_hashes.len() == self.n - 1 {
                break; // todo: add timeout handling
            }
        }

        for (index, remote_echo) in echo_hashes.into_iter().enumerate() {
            match remote_echo {
                Ok((peer_id, hash)) => {
                    if hash != echo_hash {
                        println!(
                            "Echo: Hash mismatch! Local hash (first 8 bytes): {:02x?}, Remote hash (first 8 bytes): {:02x?}, Peer: {}",
                            &echo_hash[..8.min(echo_hash.len())],
                            &hash[..8.min(hash.len())],
                            peer_id
                        );
                        return Err(crate::Error::InconsistentEcho(index as u16));
                    } else {
                        println!("Echo: Hash match with peer {}", peer_id);
                    }
                }
                Err(e) => {
                    println!("Echo: Failed to get echo response: {:?}", e);
                    return Err(crate::Error::EchoFailed(e));
                }
            }
        }

        self.r += 1;

        Ok(Some(self))
    }

    fn ready_message_id(&self) -> Option<u64> {
        self.msgs.iter().find_map(|(message_id, msgs)| {
            if msgs.len() == self.n
                && msgs
                    .iter()
                    .any(|msg| matches!(&msg.response, EchoResponse::Outgoing(_)))
            {
                Some(*message_id)
            } else {
                None
            }
        })
    }
}

pub(crate) struct EchoMessage {
    pub message_id: u64,
    pub sender: u16,
    pub payload: Vec<u8>,
    pub response: EchoResponse,
}

impl Eq for EchoMessage {}

impl PartialEq<Self> for EchoMessage {
    fn eq(&self, other: &Self) -> bool {
        self.sender == other.sender
    }
}

impl PartialOrd<Self> for EchoMessage {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.sender.cmp(&other.sender))
    }
}

impl Ord for EchoMessage {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sender.cmp(&other.sender)
    }
}

pub(crate) enum EchoResponse {
    Incoming(oneshot::Sender<request_responses::OutgoingResponse>),
    Outgoing(mpsc::Receiver<Result<(PeerId, Vec<u8>), request_responses::RequestFailure>>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future;

    #[tokio::test]
    async fn wrap_execution_waits_for_echo_after_computation_completes() {
        let (gadget, mut echo_tx) = EchoGadget::new(2);
        let (incoming_ack_tx, incoming_ack_rx) = oneshot::channel();
        let (mut outgoing_resp_tx, outgoing_resp_rx) = mpsc::channel(1);
        let remote_payload = b"remote".to_vec();
        let local_payload = b"local".to_vec();

        let mut hasher = Blake2s256::new();
        let _ = hasher.write(&remote_payload);
        let _ = hasher.write(&local_payload);
        let expected_hash = hasher.finalize().to_vec();
        let expected_hash_for_task = expected_hash.clone();

        assert!(echo_tx
            .try_send(EchoMessage {
                message_id: 0,
                sender: 1,
                payload: remote_payload,
                response: EchoResponse::Incoming(incoming_ack_tx),
            })
            .is_ok());
        assert!(echo_tx
            .try_send(EchoMessage {
                message_id: 0,
                sender: 2,
                payload: local_payload,
                response: EchoResponse::Outgoing(outgoing_resp_rx),
            })
            .is_ok());
        drop(echo_tx);

        let ack_task = tokio::spawn(async move {
            let ack = incoming_ack_rx.await.expect("echo ack should be sent");
            assert_eq!(
                ack.result.expect("echo hash should be ok"),
                expected_hash_for_task
            );
            outgoing_resp_tx
                .try_send(Ok((PeerId::random(), expected_hash)))
                .expect("outgoing response receiver should still be alive");
        });

        assert!(gadget.wrap_execution(future::ready(Ok(()))).await.is_ok());
        ack_task.await.expect("ack task should complete");
    }
}
