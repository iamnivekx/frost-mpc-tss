use crate::coordination::{LocalRpcMsg, Phase1Channel};
use crate::network_proxy::ReceiverProxy;
use crate::peerset::Peerset;
use crate::{ComputeAgentAsync, PeersetMsg};
use futures::channel::{mpsc, oneshot};
use futures::Stream;
use futures_util::stream::FuturesOrdered;
use futures_util::FutureExt;
use libp2p::PeerId;
use mpc_network::{
    request_responses, request_responses::MessageContext, request_responses::MessageType,
    NetworkService, RoomId,
};
use std::borrow::BorrowMut;
use std::collections::HashSet;
use std::future::Future;
use std::io::{BufReader, BufWriter, Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use std::{io, iter};
use tracing::debug;

pub(crate) struct NegotiationChannel {
    rx: Option<mpsc::Receiver<request_responses::IncomingRequest>>,
    timeout: Pin<Box<dyn Future<Output = ()> + Send>>,
    agent: Option<Box<dyn ComputeAgentAsync>>,
    state: Option<NegotiationState>,
}

struct NegotiationState {
    id: RoomId,
    n: u16,
    request: Vec<u8>,
    network_service: NetworkService,
    peers: HashSet<PeerId>,
    responses: Option<mpsc::Receiver<Result<(PeerId, Vec<u8>), request_responses::RequestFailure>>>,
    pending_futures: FuturesOrdered<Pin<Box<dyn Future<Output = ()> + Send>>>,
    pending_response: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    last_broadcast_time: Option<std::time::Instant>,
    retry_count: u32,
    connection_wait_start: Option<std::time::Instant>,
    connection_wait_done: bool,
    connected_peers_probe: Option<Pin<Box<dyn Future<Output = HashSet<PeerId>> + Send>>>,
    last_connected_peers_probe_at: Option<std::time::Instant>,
}

fn should_proceed_with_negotiation(
    n: u16,
    elapsed: Duration,
    connected_peers: usize,
    wait_timeout: Duration,
) -> bool {
    connected_peers + 1 >= n as usize || elapsed >= wait_timeout
}

impl NegotiationChannel {
    pub fn new(
        room_id: RoomId,
        room_rx: mpsc::Receiver<request_responses::IncomingRequest>,
        n: u16,
        request: Vec<u8>,
        network_service: NetworkService,
        agent: Box<dyn ComputeAgentAsync>,
        pending_response: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    ) -> Self {
        let local_peer_id = network_service.local_peer_id();
        Self {
            rx: Some(room_rx),
            timeout: Box::pin(tokio::time::sleep(Duration::from_secs(45))),
            agent: Some(agent),
            state: Some(NegotiationState {
                id: room_id,
                n,
                request,
                network_service,
                peers: iter::once(local_peer_id).collect(),
                responses: None,
                pending_futures: Default::default(),
                pending_response,
                last_broadcast_time: None,
                retry_count: 0,
                connection_wait_start: None,
                connection_wait_done: false,
                connected_peers_probe: None,
                last_connected_peers_probe_at: None,
            }),
        }
    }
}

impl Future for NegotiationChannel {
    type Output = NegotiationMsg;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let NegotiationState {
            id,
            n,
            request,
            network_service: service,
            mut peers,
            mut responses,
            mut pending_futures,
            pending_response,
            mut last_broadcast_time,
            mut retry_count,
            mut connection_wait_start,
            mut connection_wait_done,
            mut connected_peers_probe,
            mut last_connected_peers_probe_at,
        } = self.state.take().unwrap();

        loop {
            if let Poll::Ready(None) =
                Stream::poll_next(Pin::new(&mut pending_futures).as_mut(), cx)
            {
                break;
            }
        }

        if let Some(rx) = responses.borrow_mut() {
            match Stream::poll_next(Pin::new(rx), cx) {
                Poll::Ready(Some(Ok((peer_id, _)))) => {
                    println!("Negotiation: Received response from peer {}", peer_id);
                    peers.insert(peer_id);
                    println!("Negotiation: Collected {}/{} peers so far", peers.len(), n);
                    if peers.len() == n as usize {
                        let agent = self.agent.take().unwrap();
                        let peers_iter = peers.clone().into_iter();
                        let (peerset, peerset_rx) =
                            Peerset::new(peers_iter, service.local_peer_id());
                        let start_msg = StartMsg {
                            peerset: peerset.clone(),
                            body: request.clone(),
                        };
                        pending_futures.push_back(
                            service
                                .clone()
                                .multicast_message_owned(
                                    id.clone(),
                                    peers.clone().into_iter(),
                                    MessageContext {
                                        message_type: MessageType::Coordination,
                                        protocol_id: agent.protocol_id(),
                                    },
                                    start_msg.to_bytes().unwrap(),
                                    None,
                                )
                                .boxed(),
                        );

                        loop {
                            if let Poll::Ready(None) =
                                Stream::poll_next(Pin::new(&mut pending_futures).as_mut(), cx)
                            {
                                break;
                            }
                        }

                        let (receiver_proxy, room_receiver) = ReceiverProxy::new(
                            id.clone(),
                            self.rx.take().unwrap(),
                            service.clone(),
                            peerset.clone(),
                        );
                        return Poll::Ready(NegotiationMsg::Start {
                            agent,
                            pending_response,
                            room_receiver,
                            receiver_proxy,
                            peerset,
                            peerset_rx,
                            request,
                        });
                    }
                }
                Poll::Ready(Some(Err(_))) => {
                    // Request failed, continue
                }
                Poll::Ready(None) => {
                    // Channel closed, remove it
                    let _ = responses.take();
                }
                Poll::Pending => {
                    // No response yet, continue
                }
            }
        } else {
            let agent = self.agent.as_ref().unwrap();

            // Before first broadcast, wait for network connections to be established
            // Actively check connected peers instead of just waiting
            if !connection_wait_done && last_broadcast_time.is_none() && peers.len() < n as usize {
                if connection_wait_start.is_none() {
                    connection_wait_start = Some(std::time::Instant::now());
                    println!("Negotiation: Waiting for network connections to be established...");
                }

                // Wait until peers are connected or timeout is reached.
                let wait_timeout = Duration::from_secs(30);
                if let Some(start) = connection_wait_start {
                    let probe_interval = Duration::from_millis(250);

                    if connected_peers_probe.is_none()
                        && last_connected_peers_probe_at
                            .map_or(true, |last_probe| last_probe.elapsed() >= probe_interval)
                    {
                        let service_clone = service.clone();
                        connected_peers_probe =
                            Some(async move { service_clone.get_connected_peers().await }.boxed());
                        last_connected_peers_probe_at = Some(std::time::Instant::now());
                    }

                    if let Some(probe_fut) = connected_peers_probe.as_mut() {
                        if let Poll::Ready(connected_peers) = Future::poll(probe_fut.as_mut(), cx) {
                            let connected_count = connected_peers.len();
                            let elapsed = start.elapsed();
                            connection_wait_done = should_proceed_with_negotiation(
                                n,
                                elapsed,
                                connected_count,
                                wait_timeout,
                            );
                            connected_peers_probe = None;

                            if connection_wait_done {
                                if elapsed >= wait_timeout && connected_count + 1 < n as usize {
                                    debug!(
                                        "Negotiation: Connection wait timeout after {:?}, proceeding anyway (connected {}/{})",
                                        elapsed,
                                        connected_count + 1,
                                        n
                                    );
                                } else {
                                    debug!(
                                        "Negotiation: Network ready (connected {}/{}), proceeding with broadcast",
                                        connected_count + 1,
                                        n
                                    );
                                }
                            }
                        }
                    }
                }
            } else {
                connection_wait_done = true;
            }

            let should_broadcast = match last_broadcast_time {
                None => connection_wait_done, // Only broadcast after connection wait is done
                Some(last_time) => {
                    // Retry if it's been more than 1 second and we don't have enough peers (more aggressive retry)
                    let elapsed = last_time.elapsed();
                    elapsed >= Duration::from_secs(1) && peers.len() < n as usize
                }
            };

            if should_broadcast {
                let is_first_broadcast = last_broadcast_time.is_none();
                println!(
                    "Negotiation: Starting negotiation for room {:?}, need {} peers (already have {})",
                    id,
                    n,
                    peers.len()
                );
                let (tx, rx) = mpsc::channel((n - 1) as usize);
                pending_futures.push_back(
                    service
                        .clone()
                        .broadcast_message_owned(
                            id.clone(),
                            MessageContext {
                                message_type: MessageType::Coordination,
                                protocol_id: agent.protocol_id(),
                            },
                            vec![],
                            Some(tx),
                        )
                        .boxed(),
                );
                let _ = responses.insert(rx);
                last_broadcast_time = Some(std::time::Instant::now());
                retry_count += 1;
                if is_first_broadcast {
                    // Connection warmup can consume part of the initial timer.
                    // Reset timeout on first outbound broadcast so negotiation retries
                    // have a full timeout budget.
                    self.timeout = Box::pin(tokio::time::sleep(Duration::from_secs(15)));
                }
                println!(
                    "Negotiation: Broadcast message sent (retry #{}), waiting for responses",
                    retry_count
                );
            }
        }

        // It took too long for peerset to be assembled  - reset to Phase 1.
        if let Poll::Ready(()) = Future::poll(self.timeout.as_mut(), cx) {
            println!(
                "Negotiation: Timeout! Collected {}/{} peers. Aborting negotiation.",
                peers.len(),
                n
            );
            let (ch, tx) = Phase1Channel::new(id.clone(), self.rx.take().unwrap(), service.clone());
            return Poll::Ready(NegotiationMsg::Abort {
                room_id: id.clone(),
                phase1: ch,
                rpc_tx: tx,
                pending_response: Some(pending_response),
            });
        }

        let _ = self.state.insert(NegotiationState {
            id,
            n,
            request,
            network_service: service,
            peers,
            responses,
            pending_futures,
            pending_response,
            last_broadcast_time,
            retry_count,
            connection_wait_start,
            connection_wait_done,
            connected_peers_probe,
            last_connected_peers_probe_at,
        });

        // Wake this task to be polled again.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub(crate) enum NegotiationMsg {
    Start {
        agent: Box<dyn ComputeAgentAsync>,
        pending_response: oneshot::Sender<anyhow::Result<Vec<u8>>>,
        room_receiver: mpsc::Receiver<request_responses::IncomingRequest>,
        receiver_proxy: ReceiverProxy,
        peerset: Peerset,
        peerset_rx: mpsc::Receiver<PeersetMsg>,
        request: Vec<u8>,
    },
    Abort {
        room_id: RoomId,
        phase1: Phase1Channel,
        rpc_tx: oneshot::Sender<LocalRpcMsg>,
        pending_response: Option<oneshot::Sender<anyhow::Result<Vec<u8>>>>,
    },
}

pub(crate) struct StartMsg {
    pub peerset: Peerset,
    pub body: Vec<u8>,
}

impl StartMsg {
    pub(crate) fn from_bytes(
        b: &[u8],
        local_peer_id: PeerId,
    ) -> io::Result<(Self, mpsc::Receiver<PeersetMsg>)> {
        let mut io = BufReader::new(b);

        // Read the peerset payload length.
        let peerset_len = unsigned_varint::io::read_usize(&mut io)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        let mut peerset_buffer = vec![0; peerset_len];
        io.read_exact(&mut peerset_buffer)?;

        // Read the body payload length.
        let length = unsigned_varint::io::read_usize(&mut io)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        // Read the init message body.
        let mut body = vec![0; length];
        io.read_exact(&mut body)?;

        let (peerset, rx) = Peerset::from_bytes(&*peerset_buffer, local_peer_id)?;
        Ok((Self { peerset, body }, rx))
    }

    fn to_bytes(self) -> io::Result<Vec<u8>> {
        let b = vec![];
        let mut io = BufWriter::new(b);

        let peerset_bytes = self.peerset.to_bytes()?;

        // Write the peerset payload size.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                peerset_bytes.len(),
                &mut buffer,
            ))?;
        }

        io.write_all(&*peerset_bytes)?;

        // Write the body payload length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(self.body.len(), &mut buffer))?;
        }

        // Write the init message.
        io.write_all(&self.body)?;

        Ok(io.buffer().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use crate::negotiation::{should_proceed_with_negotiation, StartMsg};
    use crate::peerset::Peerset;
    use libp2p::PeerId;
    use std::str::FromStr;
    use std::time::Duration;

    #[test]
    fn start_msg_encoding() {
        let peer_ids = vec![
            PeerId::from_str("12D3KooWMQmcJA5raTtuxqAguM5CiXRhEDumLNmZQ7PmKZizjFBX").unwrap(),
            PeerId::from_str("12D3KooWS4jk2BXKgyqygNEZScHSzntTKQCdHYiHRrZXiNE9mNHi").unwrap(),
            PeerId::from_str("12D3KooWHYG3YsVs9hTwbgPKVrTrPQBKc8FnDhV6bsJ4W37eds8p").unwrap(),
        ];
        let local_peer_id = peer_ids[0];
        let (mut peerset, _) = Peerset::new(peer_ids.into_iter(), local_peer_id);
        peerset.parties = vec![1, 2];
        let start_msg = StartMsg {
            peerset: peerset.clone(),
            body: vec![1, 2, 3],
        };
        let encoded = StartMsg {
            peerset: peerset.clone(),
            body: vec![1, 2, 3],
        }
        .to_bytes()
        .unwrap();
        let (decoded, _) = StartMsg::from_bytes(&*encoded, local_peer_id).unwrap();

        println!(
            "original: {:?}, {:?}",
            start_msg.peerset.parties,
            start_msg.peerset.clone().remotes_iter().collect::<Vec<_>>()
        );
        println!(
            "decoded: {:?}, {:?}",
            decoded.peerset.parties,
            decoded.peerset.clone().remotes_iter().collect::<Vec<_>>()
        );

        assert_eq!(start_msg.peerset.parties, decoded.peerset.parties);
    }

    #[test]
    fn negotiation_waits_until_connections_ready() {
        assert!(
            !should_proceed_with_negotiation(3, Duration::from_secs(3), 0, Duration::from_secs(10)),
            "should keep waiting when connected peers are insufficient and timeout not reached"
        );
    }

    #[test]
    fn negotiation_proceeds_when_connections_ready() {
        assert!(should_proceed_with_negotiation(
            3,
            Duration::from_secs(1),
            2,
            Duration::from_secs(10)
        ));
    }

    #[test]
    fn negotiation_proceeds_when_wait_timeout_reached() {
        assert!(should_proceed_with_negotiation(
            3,
            Duration::from_secs(10),
            0,
            Duration::from_secs(10)
        ));
    }
}
