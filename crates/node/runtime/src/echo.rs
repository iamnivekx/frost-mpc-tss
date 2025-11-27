use blake2::{Blake2s256, Digest};
use futures::channel::{mpsc, oneshot};
use futures_util::{pin_mut, select, FutureExt, StreamExt};
use libp2p::PeerId;
use mpc_network::request_responses;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::future::Future;
use std::io::Write;

pub(crate) struct EchoGadget {
    r: u16,
    n: usize,
    msgs: BinaryHeap<EchoMessage>,
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

        loop {
            select! {
                echo_res = echo => match echo_res {
                    Ok(s) => {
                        echo = Box::pin(s.proceed_round().fuse());
                    },
                    Err(e) => {
                        // Echo failed - return the error
                        // ProtocolExecution will handle sending the error response
                        return Err(e);
                    }
                },
                comp_res = future => {
                    return comp_res;
                }
            }
        }
    }

    async fn proceed_round(&mut self) -> crate::Result<&mut Self> {
        loop {
            let msg = self.rx.select_next_some().await;
            self.msgs.push(msg);
            if self.msgs.len() == self.n {
                break;
            }
        }

        // Sort messages by sender to ensure consistent ordering
        let mut msgs_vec: Vec<_> = self.msgs.drain().collect();
        msgs_vec.sort_by_key(|m| m.sender);

        println!(
            "Echo: Collected {} messages from senders: {:?}",
            msgs_vec.len(),
            msgs_vec.iter().map(|m| m.sender).collect::<Vec<_>>()
        );

        let mut hasher = Blake2s256::new();
        let mut incoming_acks = vec![];
        let mut outgoing_resp_rx = None;

        for echo_msg in msgs_vec {
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
                            peer_id,
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

        return Ok(self);
    }
}

pub(crate) struct EchoMessage {
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
