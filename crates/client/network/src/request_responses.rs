// This file was a part of Substrate.
// broadcast.rc <> request_response.rc

// Copyright (C) 2019-2022 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
pub use libp2p::request_response::{InboundFailure, InboundRequestId, OutboundRequestId};

/// Possible failures occurring in the context of sending an outbound request and receiving the
/// response.
#[derive(Debug, Clone, thiserror::Error)]
pub enum OutboundFailure {
    /// The request could not be sent because a dialing attempt failed.
    #[error("Failed to dial the requested peer")]
    DialFailure,
    /// The request timed out before a response was received.
    #[error("Timeout while waiting for a response")]
    Timeout,
    /// The connection closed before a response was received.
    #[error("Connection was closed before a response was received")]
    ConnectionClosed,
    /// The remote supports none of the requested protocols.
    #[error("The remote supports none of the requested protocols")]
    UnsupportedProtocols,
    /// An IO failure happened on an outbound stream.
    #[error("An IO failure happened on an outbound stream")]
    Io(Arc<io::Error>),
}

impl From<libp2p::request_response::OutboundFailure> for OutboundFailure {
    fn from(out: libp2p::request_response::OutboundFailure) -> Self {
        match out {
            libp2p::request_response::OutboundFailure::DialFailure => OutboundFailure::DialFailure,
            libp2p::request_response::OutboundFailure::Timeout => OutboundFailure::Timeout,
            libp2p::request_response::OutboundFailure::ConnectionClosed => {
                OutboundFailure::ConnectionClosed
            }
            libp2p::request_response::OutboundFailure::UnsupportedProtocols => {
                OutboundFailure::UnsupportedProtocols
            }
            libp2p::request_response::OutboundFailure::Io(error) => {
                OutboundFailure::Io(Arc::new(error))
            }
        }
    }
}
use libp2p::{
    core::{transport::PortUse, Endpoint, Multiaddr},
    request_response::{
        self, Behaviour, Codec, Event as RequestResponseEvent, Message, ProtocolSupport,
        ResponseChannel,
    },
    swarm::{
        behaviour::FromSwarm, handler::multi::MultiHandler, ConnectionDenied, ConnectionId,
        NetworkBehaviour, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
    PeerId,
};
use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
    io, iter,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tracing::{debug, error, warn};

/// Logging target for the file.
const LOG_TARGET: &str = "sub-libp2p::request-response";

/// Periodically check if requests are taking too long.
const PERIODIC_REQUEST_CHECK: Duration = Duration::from_secs(2);

/// Configuration for a single request-response protocol.
#[derive(Debug, Clone)]
pub struct ProtocolConfig {
    /// Name of the protocol on the wire. Should be something like `/foo/bar`.
    pub name: Cow<'static, str>,

    /// Maximum allowed size, in bytes, of a request.
    ///
    /// Any request larger than this value will be declined as a way to avoid allocating too
    /// much memory for it.
    pub max_request_size: u64,

    /// Maximum allowed size, in bytes, of a response.
    ///
    /// Any response larger than this value will be declined as a way to avoid allocating too
    /// much memory for it.
    pub max_response_size: u64,

    /// Duration after which emitted requests are considered timed out.
    ///
    /// If you expect the response to come back quickly, you should set this to a smaller duration.
    pub request_timeout: Duration,

    /// Channel on which the networking service will send incoming messages.
    pub inbound_queue: Option<mpsc::Sender<IncomingRequest>>,
}

impl ProtocolConfig {
    pub fn new(
        name: Cow<'static, str>,
        inbound_queue: Option<mpsc::Sender<IncomingRequest>>,
    ) -> Self {
        Self {
            name,
            max_request_size: 8 * 1024 * 1024,
            max_response_size: 10 * 1024,
            request_timeout: Duration::from_secs(30),
            inbound_queue,
        }
    }
}

/// A single request received by a peer on a request-response protocol.
#[derive(Debug)]
pub struct IncomingRequest {
    /// Who sent the request.
    pub peer_id: PeerId,

    /// Who sent the request.
    pub peer_index: u16,

    /// Request sent by the remote. Will always be smaller than
    /// [`ProtocolConfig::max_request_size`].
    pub payload: Vec<u8>,

    /// Message send to all peers in the room.
    pub is_broadcast: bool,

    /// Channel to send back the response.
    ///
    /// There are two ways to indicate that handling the request failed:
    ///
    /// 1. Drop `pending_response` and thus not changing the reputation of the peer.
    ///
    /// 2. Sending an `Err(())` via `pending_response`, optionally including reputation changes for
    /// the given peer.
    pub pending_response: oneshot::Sender<OutgoingResponse>,

    /// Protocol execution context.
    pub context: MessageContext,
}

/// Response for an incoming request to be send by a request protocol handler.
#[derive(Debug)]
pub struct OutgoingResponse {
    /// The payload of the response.
    ///
    /// `Err(())` if none is available e.g. due an error while handling the request.
    pub result: Result<Vec<u8>, ()>,

    /// If provided, the `oneshot::Sender` will be notified when the request has been sent to the
    /// peer.
    ///
    /// > **Note**: Operating systems typically maintain a buffer of a few dozen kilobytes of
    /// >			outgoing data for each TCP socket, and it is not possible for a user
    /// >			application to inspect this buffer. This channel here is not actually notified
    /// >			when the response has been fully sent out, but rather when it has fully been
    /// >			written to the buffer managed by the operating system.
    pub sent_feedback: Option<oneshot::Sender<()>>,
}

/// Event generated by the [`RequestResponsesBehaviour`].
#[derive(Debug)]
pub enum Event {
    /// A remote sent a request and either we have successfully answered it or an error happened.
    ///
    /// This event is generated for statistics purposes.
    InboundRequest {
        /// Peer which has emitted the request.
        peer: PeerId,
        /// Name of the protocol in question.
        protocol: Cow<'static, str>,
        /// Whether handling the request was successful or unsuccessful.
        ///
        /// When successful contains the time elapsed between when we received the request and when
        /// we sent back the response. When unsuccessful contains the failure reason.
        result: Result<Duration, ResponseFailure>,
    },

    /// A request initiated using [`RequestResponsesBehaviour::send_request`] has succeeded or
    /// failed.
    ///
    /// This event is generated for statistics purposes.
    RequestFinished {
        /// Peer that we send a request to.
        peer: PeerId,
        /// Name of the protocol in question.
        protocol: Cow<'static, str>,
        /// Duration the request took.
        duration: Duration,
        /// Result of the request.
        result: Result<(), RequestFailure>,
    },
}

/// Combination of a protocol name and a request id.
///
/// Uniquely identifies an inbound or outbound request among all handled protocols. Note however
/// that uniqueness is only guaranteed between two inbound and likewise between two outbound
/// requests. There is no uniqueness guarantee in a set of both inbound and outbound
/// [`ProtocolRequestId`]s.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProtocolRequestId<RequestId> {
    protocol: Cow<'static, str>,
    request_id: RequestId,
}

impl<RequestId> From<(Cow<'static, str>, RequestId)> for ProtocolRequestId<RequestId> {
    fn from((protocol, request_id): (Cow<'static, str>, RequestId)) -> Self {
        Self {
            protocol,
            request_id,
        }
    }
}

/// When sending a request, what to do on a disconnected recipient.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum IfDisconnected {
    /// Try to connect to the peer.
    TryConnect,
    /// Just fail if the destination is not yet connected.
    ImmediateError,
}

/// Convenience functions for `IfDisconnected`.
impl IfDisconnected {
    /// Shall we connect to a disconnected peer?
    pub fn should_connect(self) -> bool {
        match self {
            Self::TryConnect => true,
            Self::ImmediateError => false,
        }
    }
}

// This is a state of processing incoming request Message.
// The main reason of this struct is to hold `get_peer_reputation` as a Future state.
struct MessageRequest {
    peer: PeerId,
    request_id: InboundRequestId,
    request: WireMessage,
    channel: ResponseChannel<Result<Vec<u8>, ()>>,
    protocol: String,
    resp_builder: Option<mpsc::Sender<IncomingRequest>>,
    // Once we get incoming request we save all params, create an async call to Peerset
    // to get the reputation of the peer.
    // get_peer_reputation: Pin<Box<dyn Future<Output = Result<i32, ()>> + Send>>,
    // get_peer_index: Pin<Box<dyn Future<Output = Result<u16, ()>> + Send>>,
}

/// Information stored about a pending request.
struct PendingRequest {
    /// The time when the request was sent to the libp2p request-response protocol.
    started_at: Instant,
    /// The channel to send the response back to the caller.
    ///
    /// This is wrapped in an `Option` to allow for the channel to be taken out
    /// on force-detected timeouts.
    response_tx: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
}

/// Details of a request-response protocol.
struct ProtocolDetails {
    behaviour: Behaviour<GenericCodec>,
    inbound_queue: Option<mpsc::Sender<IncomingRequest>>,
    request_timeout: Duration,
}

/// Generated by the response builder and waiting to be processed.
struct RequestProcessingOutcome {
    peer: PeerId,
    request_id: InboundRequestId,
    protocol: Cow<'static, str>,
    inner_channel: ResponseChannel<Result<Vec<u8>, ()>>,
    response: OutgoingResponse,
}

/// Implementation of `NetworkBehaviour` that provides support for broadcast protocols.
pub struct RequestResponsesBehaviour {
    /// The multiple sub-protocols, by name.
    ///
    /// Contains the underlying libp2p request-response [`Behaviour`], plus an optional
    /// "response builder" used to build responses for incoming requests.
    protocols: HashMap<Cow<'static, str>, ProtocolDetails>,

    /// Pending requests, passed down to a request-response [`Behaviour`], awaiting a reply.
    pending_requests: HashMap<ProtocolRequestId<OutboundRequestId>, PendingRequest>,

    /// Whenever an incoming request arrives, a `Future` is added to this list and will yield the
    /// start time and the response to send back to the remote.
    pending_responses: stream::FuturesUnordered<
        Pin<Box<dyn Future<Output = Option<RequestProcessingOutcome>> + Send>>,
    >,

    /// Whenever an incoming request arrives, the arrival [`Instant`] is recorded here.
    pending_responses_arrival_time: HashMap<ProtocolRequestId<InboundRequestId>, Instant>,

    /// Whenever a response is received on `pending_responses`, insert a channel to be notified
    /// when the request has been sent out.
    send_feedback: HashMap<ProtocolRequestId<InboundRequestId>, oneshot::Sender<()>>,

    /// Pending message request, holds `MessageRequest` as a Future state to poll it
    /// until we get a response from `Peerset`
    message_request: Option<MessageRequest>,

    /// Interval to check that the requests are not taking too long.
    ///
    /// We had issues in the past where libp2p did not produce a timeout event in due time.
    ///
    /// For more details, see:
    /// - <https://github.com/paritytech/polkadot-sdk/issues/7076#issuecomment-2596085096>
    periodic_request_check: tokio::time::Interval,
}

impl RequestResponsesBehaviour {
    /// Creates a new behaviour. Must be passed a list of supported protocols. Returns an error if
    /// the same protocol is passed twice.
    pub fn new(list: impl Iterator<Item = ProtocolConfig>) -> Result<Self, RegisterError> {
        let mut protocols = HashMap::new();
        for protocol in list {
            let cfg =
                request_response::Config::default().with_request_timeout(protocol.request_timeout);

            let protocol_support = if protocol.inbound_queue.is_some() {
                ProtocolSupport::Full
            } else {
                ProtocolSupport::Outbound
            };

            // Convert protocol.name to a static string to avoid lifetime issues
            // StreamProtocol::new takes &str and stores it internally, so we need
            // a string that lives long enough. Converting to String and then to &'static str
            let protocol_name = protocol.name.clone();
            let protocol_name_static: &'static str =
                Box::leak(protocol_name.as_ref().to_string().into_boxed_str());
            let protocol_stream = libp2p::swarm::StreamProtocol::new(protocol_name_static);
            let rq_rp = Behaviour::with_codec(
                GenericCodec {
                    max_request_size: protocol.max_request_size,
                    max_response_size: protocol.max_response_size,
                },
                iter::once((protocol_stream, protocol_support)),
                cfg,
            );

            // Now we can safely move protocol_name into entry
            match protocols.entry(protocol_name) {
                Entry::Vacant(e) => e.insert(ProtocolDetails {
                    behaviour: rq_rp,
                    inbound_queue: protocol.inbound_queue,
                    request_timeout: protocol.request_timeout,
                }),
                Entry::Occupied(e) => {
                    return Err(RegisterError::DuplicateProtocol(e.key().clone()))
                }
            };
        }

        Ok(Self {
            protocols,
            pending_requests: Default::default(),
            pending_responses: Default::default(),
            pending_responses_arrival_time: Default::default(),
            send_feedback: Default::default(),
            message_request: None,
            periodic_request_check: tokio::time::interval(PERIODIC_REQUEST_CHECK),
        })
    }

    /// Initiates sending a request.
    ///
    /// If there is no established connection to the target peer, the behavior is determined by the
    /// choice of `connect`.
    ///
    /// An error is returned if the protocol doesn't match one that has been registered.
    pub fn send_request(
        &mut self,
        target: &PeerId,
        protocol_name: &str,
        ctx: MessageContext,
        payload: Vec<u8>,
        pending_response: mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>,
        connect: IfDisconnected,
    ) {
        self.send_request_message(
            target,
            protocol_name,
            WireMessage {
                is_broadcast: false,
                payload,
                context: ctx,
            },
            Some(pending_response),
            connect,
        );
    }

    pub fn broadcast_message(
        &mut self,
        targets: impl Iterator<Item = PeerId>,
        protocol_name: &str,
        ctx: MessageContext,
        payload: Vec<u8>,
        pending_response: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
        connect: IfDisconnected,
    ) {
        for target in targets {
            self.send_request_message(
                &target,
                protocol_name,
                WireMessage {
                    is_broadcast: true,
                    payload: payload.clone(),
                    context: ctx,
                },
                pending_response.clone(),
                connect,
            );
        }
    }

    fn send_request_message(
        &mut self,
        target: &PeerId,
        protocol_name: &str,
        message: WireMessage,
        pending_response: Option<mpsc::Sender<Result<(PeerId, Vec<u8>), RequestFailure>>>,
        connect: IfDisconnected,
    ) {
        if let Some(ProtocolDetails { behaviour, .. }) = self.protocols.get_mut(protocol_name) {
            if behaviour.is_connected(target) || connect.should_connect() {
                let request_id = behaviour.send_request(target, message);
                let prev_req_id = self.pending_requests.insert(
                    (protocol_name.to_string().into(), request_id).into(),
                    PendingRequest {
                        started_at: Instant::now(),
                        response_tx: pending_response,
                    },
                );
                debug_assert!(prev_req_id.is_none(), "Expect request id to be unique.");
            } else {
                if let Some(mut pending_response) = pending_response {
                    if pending_response
                        .try_send(Err(RequestFailure::NotConnected))
                        .is_err()
                    {
                        debug!(
                            target: LOG_TARGET,
                            "Not connected to peer {:?}. At the same time local \
                             node is no longer interested in the result.",
                            target,
                        );
                    };
                }
            }
        } else {
            if let Some(mut pending_response) = pending_response {
                if pending_response
                    .try_send(Err(RequestFailure::UnknownProtocol))
                    .is_err()
                {
                    debug!(
                        target: LOG_TARGET,
                        "Unknown protocol {:?}. At the same time local \
                         node is no longer interested in the result.",
                        protocol_name,
                    );
                };
            }
        }
    }
}

impl NetworkBehaviour for RequestResponsesBehaviour {
    type ConnectionHandler =
        MultiHandler<String, <Behaviour<GenericCodec> as NetworkBehaviour>::ConnectionHandler>;
    type ToSwarm = Event;

    fn handle_pending_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        Ok(())
    }

    fn handle_pending_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _maybe_peer: Option<PeerId>,
        _addresses: &[Multiaddr],
        _effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        Ok(Vec::new())
    }

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let iter =
            self.protocols
                .iter_mut()
                .filter_map(|(p, ProtocolDetails { behaviour, .. })| {
                    if let Ok(handler) = behaviour.handle_established_inbound_connection(
                        connection_id,
                        peer,
                        local_addr,
                        remote_addr,
                    ) {
                        Some((p.to_string(), handler))
                    } else {
                        None
                    }
                });

        Ok(MultiHandler::try_from_iter(iter).expect(
            "Protocols are in a HashMap and there can be at most one handler per protocol name, \
                which is the only possible error; qed",
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let iter =
            self.protocols
                .iter_mut()
                .filter_map(|(p, ProtocolDetails { behaviour, .. })| {
                    if let Ok(handler) = behaviour.handle_established_outbound_connection(
                        connection_id,
                        peer,
                        addr,
                        role_override,
                        port_use,
                    ) {
                        Some((p.to_string(), handler))
                    } else {
                        None
                    }
                });

        Ok(MultiHandler::try_from_iter(iter).expect(
            "Protocols are in a HashMap and there can be at most one handler per protocol name, \
                which is the only possible error; qed",
        ))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        // Forward the event to all protocols
        // Note: FromSwarm events can be forwarded to multiple behaviours
        // However, some events contain references that cannot be used multiple times
        // For now, we forward to the first protocol only to avoid issues
        // The swarm should distribute events to all behaviours automatically
        if let Some(ProtocolDetails { behaviour, .. }) = self.protocols.values_mut().next() {
            behaviour.on_swarm_event(event);
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        let p_name = event.0;
        if let Some(ProtocolDetails { behaviour, .. }) = self.protocols.get_mut(p_name.as_str()) {
            behaviour.on_connection_handler_event(peer_id, connection_id, event.1);
        } else {
            warn!(
                target: LOG_TARGET,
                "on_connection_handler_event: no request-response instance registered for protocol {:?}",
                p_name
            );
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        'poll_all: loop {
            // Poll the periodic request check.
            if self.periodic_request_check.poll_tick(cx).is_ready() {
                self.pending_requests.retain(|id, req| {
                    let Some(ProtocolDetails { request_timeout, .. }) =
                        self.protocols.get(&id.protocol)
                    else {
                        warn!(
                            target: LOG_TARGET,
                            "Request {id:?} has no protocol registered.",
                        );

                        if let Some(mut response_tx) = req.response_tx.take() {
                            if response_tx
                                .try_send(Err(RequestFailure::UnknownProtocol))
                                .is_err()
                            {
                                debug!(
                                    target: LOG_TARGET,
                                    "Request {id:?} has no protocol registered. At the same time local node is no longer interested in the result.",
                                );
                            }
                        }
                        return false;
                    };

                    let elapsed = req.started_at.elapsed();
                    if elapsed > *request_timeout {
                        debug!(
                            target: LOG_TARGET,
                            "Request {id:?} force detected as timeout.",
                        );

                        if let Some(mut response_tx) = req.response_tx.take() {
                            if response_tx
                                .try_send(Err(RequestFailure::Network(
                                    OutboundFailure::Timeout,
                                )))
                                .is_err()
                            {
                                debug!(
                                    target: LOG_TARGET,
                                    "Request {id:?} force detected as timeout. At the same time local node is no longer interested in the result.",
                                );
                            }
                        }

                        false
                    } else {
                        true
                    }
                });
            }

            if let Some(message_request) = self.message_request.take() {
                let MessageRequest {
                    peer,
                    request_id,
                    request,
                    channel,
                    protocol,
                    resp_builder,
                } = message_request;

                let (tx, rx) = oneshot::channel();

                // Submit the request to the "response builder" passed by the user at
                // initialization.
                if let Some(mut resp_builder) = resp_builder {
                    // If the response builder is too busy, silently drop `tx`. This
                    // will be reported by the corresponding `RequestResponse` through
                    // an `InboundFailure::Omission` event.
                    if let Err(e) = resp_builder.try_send(IncomingRequest {
                        peer_id: peer,
                        peer_index: 0,
                        is_broadcast: request.is_broadcast,
                        payload: request.payload,
                        context: request.context,
                        pending_response: tx,
                    }) {
                        error!("Failed to send incoming request to response builder: {e}");
                    };
                }

                let protocol = Cow::from(protocol);
                self.pending_responses.push(Box::pin(async move {
                    // The `tx` created above can be dropped if we are not capable of
                    // processing this request, which is reflected as a
                    // `InboundFailure::Omission` event.
                    if let Ok(response) = rx.await {
                        Some(RequestProcessingOutcome {
                            peer,
                            request_id,
                            protocol,
                            inner_channel: channel,
                            response,
                        })
                    } else {
                        None
                    }
                }));

                // This `continue` makes sure that `pending_responses` gets polled
                // after we have added the new element.
                continue 'poll_all;
            }

            // Poll to see if any response is ready to be sent back.
            while let Poll::Ready(Some(outcome)) = self.pending_responses.poll_next_unpin(cx) {
                let RequestProcessingOutcome {
                    peer,
                    request_id,
                    protocol: protocol_name,
                    inner_channel,
                    response:
                        OutgoingResponse {
                            result,
                            sent_feedback,
                        },
                } = match outcome {
                    Some(outcome) => outcome,
                    None => continue,
                };

                if let Ok(payload) = result {
                    if let Some(ProtocolDetails { behaviour, .. }) =
                        self.protocols.get_mut(&*protocol_name)
                    {
                        debug!(
                            target: LOG_TARGET,
                            "send response to {peer} ({protocol_name:?}), {} bytes",
                            payload.len()
                        );

                        if behaviour.send_response(inner_channel, Ok(payload)).is_err() {
                            // Note: Failure is handled further below when receiving
                            // `InboundFailure` event from request-response [`Behaviour`].
                            debug!(
                                target: LOG_TARGET,
                                "Failed to send response for {:?} on protocol {:?} due to a \
                                 timeout or due to the connection to the peer being closed. \
                                 Dropping response",
                                request_id, protocol_name
                            );
                        } else if let Some(sent_feedback) = sent_feedback {
                            self.send_feedback
                                .insert((protocol_name, request_id).into(), sent_feedback);
                        }
                    }
                }
            }

            // Poll request-responses protocols.
            for (
                protocol,
                ProtocolDetails {
                    behaviour,
                    inbound_queue,
                    ..
                },
            ) in &mut self.protocols
            {
                loop {
                    match behaviour.poll(cx) {
                        Poll::Ready(ToSwarm::GenerateEvent(ev)) => {
                            match ev {
                                // Received a request from a remote.
                                RequestResponseEvent::Message {
                                    peer,
                                    message:
                                        Message::Request {
                                            request_id,
                                            request,
                                            channel,
                                            ..
                                        },
                                } => {
                                    self.pending_responses_arrival_time.insert(
                                        (protocol.clone(), request_id.clone()).into(),
                                        Instant::now(),
                                    );

                                    // Save the Future-like state with params to poll `get_peer_reputation`
                                    // and to continue processing the request once we get the reputation of
                                    // the peer.
                                    // We already have inbound_queue from the loop, so we can use it directly
                                    self.message_request = Some(MessageRequest {
                                        peer,
                                        request_id,
                                        request,
                                        channel,
                                        protocol: protocol.to_string(),
                                        resp_builder: inbound_queue.clone(),
                                    });

                                    // This `continue` makes sure that `message_request` gets polled
                                    // after we have added the new element.
                                    continue 'poll_all;
                                }

                                // Received a response from a remote to one of our requests.
                                RequestResponseEvent::Message {
                                    peer,
                                    message:
                                        Message::Response {
                                            request_id,
                                            response,
                                        },
                                    ..
                                } => {
                                    let (started, delivered) = match self
                                        .pending_requests
                                        .remove(&(protocol.clone(), request_id).into())
                                    {
                                        Some(PendingRequest {
                                            started_at,
                                            response_tx: Some(mut response_tx),
                                        }) => {
                                            debug!(
                                                target: LOG_TARGET,
                                                "received response from {peer} ({protocol:?}), {} bytes",
                                                response.as_ref().map_or(0usize, |response| response.len()),
                                            );

                                            let delivered = response_tx
                                                .try_send(
                                                    response
                                                        .map(|r| (peer.clone(), r))
                                                        .map_err(|()| RequestFailure::Refused),
                                                )
                                                .map_err(|_| RequestFailure::Obsolete);
                                            (started_at, delivered)
                                        }
                                        Some(PendingRequest {
                                            started_at,
                                            response_tx: None,
                                        }) => {
                                            // Response channel was already taken (e.g., due to timeout)
                                            (started_at, Ok(()))
                                        }
                                        None => {
                                            warn!(
                                                target: LOG_TARGET,
                                                "Received `RequestResponseEvent::Message` with unexpected request id {:?} from {:?}",
                                                request_id,
                                                peer,
                                            );
                                            debug_assert!(false);
                                            continue;
                                        }
                                    };

                                    let out = Event::RequestFinished {
                                        peer,
                                        protocol: protocol.clone(),
                                        duration: started.elapsed(),
                                        result: delivered,
                                    };

                                    return Poll::Ready(ToSwarm::GenerateEvent(out));
                                }

                                // One of our requests has failed.
                                RequestResponseEvent::OutboundFailure {
                                    peer,
                                    request_id,
                                    error,
                                    ..
                                } => {
                                    let outbound_error = OutboundFailure::from(error);
                                    let started = match self
                                        .pending_requests
                                        .remove(&(protocol.clone(), request_id).into())
                                    {
                                        Some(PendingRequest {
                                            started_at,
                                            response_tx: Some(mut response_tx),
                                        }) => {
                                            if response_tx
                                                .try_send(Err(RequestFailure::Network(
                                                    outbound_error.clone(),
                                                )))
                                                .is_err()
                                            {
                                                debug!(
                                                    target: LOG_TARGET,
                                                    "Request with id {:?} failed. At the same time local \
                                                     node is no longer interested in the result.",
                                                    request_id,
                                                );
                                            }
                                            started_at
                                        }
                                        Some(PendingRequest {
                                            started_at,
                                            response_tx: None,
                                        }) => {
                                            // Response channel was already taken (e.g., due to timeout)
                                            started_at
                                        }
                                        None => {
                                            warn!(
                                                target: LOG_TARGET,
                                                "Received `RequestResponseEvent::OutboundFailure` with unexpected request id {:?} error {:?} from {:?}",
                                                request_id,
                                                outbound_error,
                                                peer
                                            );
                                            continue;
                                        }
                                    };

                                    let out = Event::RequestFinished {
                                        peer,
                                        protocol: protocol.clone(),
                                        duration: started.elapsed(),
                                        result: Err(RequestFailure::Network(outbound_error)),
                                    };

                                    return Poll::Ready(ToSwarm::GenerateEvent(out));
                                }

                                // An inbound request failed, either while reading the request or due to
                                // failing to send a response.
                                RequestResponseEvent::InboundFailure {
                                    peer,
                                    request_id,
                                    error,
                                    ..
                                } => {
                                    self.pending_responses_arrival_time
                                        .remove(&(protocol.clone(), request_id).into());
                                    self.send_feedback
                                        .remove(&(protocol.clone(), request_id).into());
                                    let out = Event::InboundRequest {
                                        peer,
                                        protocol: protocol.clone(),
                                        result: Err(ResponseFailure::Network(error.into())),
                                    };
                                    return Poll::Ready(ToSwarm::GenerateEvent(out));
                                }

                                // A response to an inbound request has been sent.
                                RequestResponseEvent::ResponseSent { peer, request_id } => {
                                    let arrival_time = self
                                .pending_responses_arrival_time
                                .remove(&(protocol.clone(), request_id).into())
                                .map(|t| t.elapsed())
                                .expect(
                                    "Time is added for each inbound request on arrival and only \
									 removed on success (`ResponseSent`) or failure \
									 (`InboundFailure`). One can not receive a success event for a \
									 request that either never arrived, or that has previously \
									 failed; qed.",
                                );

                                    if let Some(send_feedback) = self
                                        .send_feedback
                                        .remove(&(protocol.clone(), request_id).into())
                                    {
                                        let _ = send_feedback.send(());
                                    }

                                    let out = Event::InboundRequest {
                                        peer,
                                        protocol: protocol.clone(),
                                        result: Ok(arrival_time),
                                    };

                                    return Poll::Ready(ToSwarm::GenerateEvent(out));
                                }
                            }
                        }
                        Poll::Ready(other) => {
                            // Convert ToSwarm<request_response::Event, ...> to ToSwarm<Event, ...>
                            // For non-GenerateEvent variants, we can forward them directly
                            match other {
                                ToSwarm::Dial { opts } => {
                                    return Poll::Ready(ToSwarm::Dial { opts });
                                }
                                ToSwarm::CloseConnection {
                                    peer_id,
                                    connection,
                                } => {
                                    return Poll::Ready(ToSwarm::CloseConnection {
                                        peer_id,
                                        connection,
                                    });
                                }
                                ToSwarm::NotifyHandler {
                                    peer_id,
                                    handler,
                                    event,
                                } => {
                                    // event is OutboundMessage<GenericCodec>, but we need (String, OutboundMessage<GenericCodec>)
                                    // We need to wrap it with the protocol name
                                    return Poll::Ready(ToSwarm::NotifyHandler {
                                        peer_id,
                                        handler,
                                        event: (protocol.to_string(), event),
                                    });
                                }
                                ToSwarm::GenerateEvent(_) => {
                                    // This should have been handled above
                                    unreachable!();
                                }
                                _ => {
                                    // Handle any other ToSwarm variants
                                    return Poll::Ready(other.map_in(|event| (protocol.to_string(), event)).map_out(|_| {
                                        unreachable!("`GenerateEvent` is handled in a branch above; qed")
                                    }));
                                }
                            }
                        }
                        Poll::Pending => break,
                    }
                }
            }

            break Poll::Pending;
        }
    }
}

/// Error when registering a protocol.
#[derive(thiserror::Error, Debug)]
pub enum RegisterError {
    /// A protocol has been specified multiple times.
    #[error("{0}")]
    DuplicateProtocol(Cow<'static, str>),
}

/// Error in a request.
#[derive(thiserror::Error, Debug)]
#[allow(missing_docs)]
pub enum RequestFailure {
    #[error("We are not currently connected to the requested peer.")]
    NotConnected,
    #[error("Given protocol hasn't been registered.")]
    UnknownProtocol,
    #[error("Remote has closed the substream before answering, thereby signaling that it considers the request as valid, but refused to answer it.")]
    Refused,
    #[error("The remote replied, but the local node is no longer interested in the response.")]
    Obsolete,
    /// Problem on the network.
    #[error("Problem on the network: {0}")]
    Network(OutboundFailure),
}

/// Error when processing a request sent by a remote.
#[derive(Debug, thiserror::Error)]
pub enum ResponseFailure {
    /// Problem on the network.
    #[error("Problem on the network: {0}")]
    Network(InboundFailure),
}

pub struct WireMessage {
    pub context: MessageContext,
    pub payload: Vec<u8>,
    pub is_broadcast: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct MessageContext {
    pub message_type: MessageType,
    pub protocol_id: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum MessageType {
    Coordination = 0,
    Computation,
}

/// Implements the libp2p [`Codec`] trait. Defines how streams of bytes are turned
/// into requests and responses and vice-versa.
#[derive(Debug, Clone)]
#[doc(hidden)] // Needs to be public in order to satisfy the Rust compiler.
pub struct GenericCodec {
    pub max_request_size: u64,
    pub max_response_size: u64,
}

#[async_trait::async_trait]
impl Codec for GenericCodec {
    type Protocol = libp2p::swarm::StreamProtocol;
    type Request = WireMessage;
    type Response = Result<Vec<u8>, ()>;

    async fn read_request<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let message_type = match unsigned_varint::aio::read_u8(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?
        {
            0 => MessageType::Coordination,
            1 => MessageType::Computation,
            _ => {
                panic!("unknown messages type");
            }
        };

        let is_broadcast = unsigned_varint::aio::read_u8(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        let protocol_id = unsigned_varint::aio::read_u64(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        // Read the length.
        let length = unsigned_varint::aio::read_usize(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        if length > usize::try_from(self.max_request_size).unwrap_or(usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Request size exceeds limit: {} > {}",
                    length, self.max_request_size
                ),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;

        Ok(WireMessage {
            context: MessageContext {
                message_type,
                protocol_id,
            },
            payload: buffer,
            is_broadcast: is_broadcast != 0,
        })
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        // Read the length.
        let length = match unsigned_varint::aio::read_usize(&mut io).await {
            Ok(l) => l,
            Err(unsigned_varint::io::ReadError::Io(err))
                if matches!(err.kind(), io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(Err(()));
            }
            Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidInput, err)),
        };

        if length > usize::try_from(self.max_response_size).unwrap_or(usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Response size exceeds limit: {} > {}",
                    length, self.max_response_size
                ),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;
        Ok(Ok(buffer))
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        // Write message type
        {
            let mut buffer = unsigned_varint::encode::u8_buffer();
            io.write_all(unsigned_varint::encode::u8(
                req.context.message_type as u8,
                &mut buffer,
            ))
            .await?;
        }

        // Write broadcast marker
        {
            let mut buffer = unsigned_varint::encode::u8_buffer();
            io.write_all(unsigned_varint::encode::u8(
                req.is_broadcast as u8,
                &mut buffer,
            ))
            .await?;
        }

        // Write protocol_id
        {
            let mut buffer = unsigned_varint::encode::u64_buffer();
            io.write_all(unsigned_varint::encode::u64(
                req.context.protocol_id,
                &mut buffer,
            ))
            .await?;
        }

        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                req.payload.len(),
                &mut buffer,
            ))
            .await?;
        }

        // Write the payload.
        io.write_all(&req.payload).await?;

        io.close().await?;
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        // If `res` is an `Err`, we jump to closing the substream without writing anything on it.
        if let Ok(res) = res {
            // TODO: check the length?
            // Write the length.
            {
                let mut buffer = unsigned_varint::encode::usize_buffer();
                io.write_all(unsigned_varint::encode::usize(res.len(), &mut buffer))
                    .await?;
            }

            // Write the payload.
            io.write_all(&res).await?;
        }

        io.close().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{
        channel::{mpsc, oneshot},
        executor::LocalPool,
        task::Spawn,
    };
    use libp2p::{
        core::{
            transport::{MemoryTransport, Transport},
            upgrade,
        },
        identity::Keypair,
        noise,
        swarm::{Swarm, SwarmEvent},
        Multiaddr,
    };

    use std::{iter, time::Duration};

    fn build_swarm(
        list: impl Iterator<Item = ProtocolConfig>,
    ) -> (Swarm<RequestResponsesBehaviour>, Multiaddr) {
        let keypair = Keypair::generate_ed25519();

        let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
            .into_authentic(&keypair)
            .unwrap();

        let transport = MemoryTransport
            .upgrade(upgrade::Version::V1)
            .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
            .multiplex(libp2p::yamux::YamuxConfig::default())
            .boxed();

        let behaviour = RequestResponsesBehaviour::new(list).unwrap();

        let mut swarm = Swarm::new(transport, behaviour, keypair.public().to_peer_id());
        let listen_addr: Multiaddr = format!("/memory/{}", rand::random::<u64>())
            .parse()
            .unwrap();

        swarm.listen_on(listen_addr.clone()).unwrap();
        (swarm, listen_addr)
    }

    #[test]
    fn basic_request_response_works() {
        let protocol_name = "/test/req-resp/1";
        let mut pool = LocalPool::new();

        // Build swarms whose behaviour is `RequestResponsesBehaviour`.
        let mut swarms = (0..2)
            .map(|_| {
                let (tx, mut rx) = mpsc::channel::<IncomingRequest>(64);

                pool.spawner()
                    .spawn_obj(
                        async move {
                            while let Some(rq) = rx.next().await {
                                let (fb_tx, fb_rx) = oneshot::channel::<_>();
                                assert_eq!(rq.payload, b"this is a request");
                                let _ = rq.pending_response.send(super::OutgoingResponse {
                                    result: Ok(b"this is a response".to_vec()),
                                    sent_feedback: Some(fb_tx),
                                });
                                fb_rx.await.unwrap();
                            }
                        }
                        .boxed()
                        .into(),
                    )
                    .unwrap();

                let protocol_config = ProtocolConfig {
                    name: From::from(protocol_name),
                    max_request_size: 1024,
                    max_response_size: 1024 * 1024,
                    request_timeout: Duration::from_secs(30),
                    inbound_queue: Some(tx),
                };

                build_swarm(iter::once(protocol_config))
            })
            .collect::<Vec<_>>();

        // Ask `swarm[0]` to dial `swarm[1]`. There isn't any discovery mechanism in place in
        // this test, so they wouldn't connect to each other.
        {
            let dial_addr = swarms[1].1.clone();
            Swarm::dial_addr(&mut swarms[0].0, dial_addr).unwrap();
        }

        let (mut swarm, _) = swarms.remove(0);
        // Running `swarm[0]` in the background.
        pool.spawner()
            .spawn_obj({
                async move {
                    loop {
                        match swarm.select_next_some().await {
                            SwarmEvent::Behaviour(Event::InboundRequest { result, .. }) => {
                                result.unwrap();
                            }
                            _ => {}
                        }
                    }
                }
                .boxed()
                .into()
            })
            .unwrap();

        // Remove and run the remaining swarm.
        let (mut swarm, _) = swarms.remove(0);
        pool.run_until(async move {
            let mut response_receiver = None;

            loop {
                match swarm.select_next_some().await {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        let (sender, receiver) = mpsc::channel::<_>(1);
                        let ctx = MessageContext {
                            message_type: MessageType::Computation,
                            protocol_id: 0,
                        };
                        swarm.behaviour_mut().send_request(
                            &peer_id,
                            protocol_name,
                            ctx,
                            b"this is a request".to_vec(),
                            sender,
                            IfDisconnected::ImmediateError,
                        );
                        assert!(response_receiver.is_none());
                        response_receiver = Some(receiver);
                    }
                    SwarmEvent::Behaviour(Event::RequestFinished { result, .. }) => {
                        result.unwrap();
                        break;
                    }
                    _ => {}
                }
            }
            let (_, response_data) = response_receiver.unwrap().next().await.unwrap().unwrap();
            assert_eq!(response_data, b"this is a response");
        });
    }
    #[test]
    fn max_response_size_exceeded() {
        let protocol_name = "/test/req-resp/1";
        let mut pool = LocalPool::new();

        // Build swarms whose behaviour is `RequestResponsesBehaviour`.
        let mut swarms = (0..2)
            .map(|_| {
                let (tx, mut rx) = mpsc::channel::<IncomingRequest>(64);

                pool.spawner()
                    .spawn_obj(
                        async move {
                            while let Some(rq) = rx.next().await {
                                assert_eq!(rq.payload, b"this is a request");
                                let _ = rq.pending_response.send(super::OutgoingResponse {
                                    result: Ok(b"this response exceeds the limit".to_vec()),
                                    sent_feedback: None,
                                });
                            }
                        }
                        .boxed()
                        .into(),
                    )
                    .unwrap();

                let protocol_config = ProtocolConfig {
                    name: From::from(protocol_name),
                    max_request_size: 1024,
                    max_response_size: 8, // <-- important for the test
                    request_timeout: Duration::from_secs(30),
                    inbound_queue: Some(tx),
                };

                build_swarm(iter::once(protocol_config))
            })
            .collect::<Vec<_>>();

        // Ask `swarm[0]` to dial `swarm[1]`. There isn't any discovery mechanism in place in
        // this test, so they wouldn't connect to each other.
        {
            let dial_addr = swarms[1].1.clone();
            Swarm::dial_addr(&mut swarms[0].0, dial_addr).unwrap();
        }

        // Running `swarm[0]` in the background until a `InboundRequest` event happens,
        // which is a hint about the test having ended.
        let (mut swarm, _) = swarms.remove(0);
        pool.spawner()
            .spawn_obj({
                async move {
                    loop {
                        match swarm.select_next_some().await {
                            SwarmEvent::Behaviour(Event::InboundRequest { result, .. }) => {
                                assert!(result.is_ok());
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                .boxed()
                .into()
            })
            .unwrap();

        // Remove and run the remaining swarm.
        let (mut swarm, _) = swarms.remove(0);
        pool.run_until(async move {
            let mut response_receiver = None;

            loop {
                match swarm.select_next_some().await {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        let (sender, receiver) = mpsc::channel(1);
                        let ctx = MessageContext {
                            message_type: MessageType::Computation,
                            protocol_id: 0,
                        };
                        swarm.behaviour_mut().send_request(
                            &peer_id,
                            protocol_name,
                            ctx,
                            b"this is a request".to_vec(),
                            sender,
                            IfDisconnected::ImmediateError,
                        );
                        assert!(response_receiver.is_none());
                        response_receiver = Some(receiver);
                    }
                    SwarmEvent::Behaviour(Event::RequestFinished { result, .. }) => {
                        assert!(result.is_err());
                        break;
                    }
                    _ => {}
                }
            }

            match response_receiver
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap_err()
            {
                RequestFailure::Network(OutboundFailure::ConnectionClosed) => {}
                _ => panic!(),
            }
        });
    }

    /// A [`RequestId`] is a unique identifier among either all inbound or all outbound requests for
    /// a single [`RequestResponse`] behaviour. It is not guaranteed to be unique across multiple
    /// [`RequestResponse`] behaviours. Thus when handling [`RequestId`] in the context of multiple
    /// [`RequestResponse`] behaviours, one needs to couple the protocol name with the [`RequestId`]
    /// to get a unique request identifier.
    ///
    /// This test ensures that two requests on different protocols can be handled concurrently
    /// without a [`RequestId`] collision.
    ///
    /// See [`ProtocolRequestId`] for additional information.
    #[test]
    fn request_id_collision() {
        let protocol_name_1 = "/test/req-resp-1/1";
        let protocol_name_2 = "/test/req-resp-2/1";
        let mut pool = LocalPool::new();

        let mut swarm_1 = {
            let protocol_configs = vec![
                ProtocolConfig {
                    name: From::from(protocol_name_1),
                    max_request_size: 1024,
                    max_response_size: 1024 * 1024,
                    request_timeout: Duration::from_secs(30),
                    inbound_queue: None,
                },
                ProtocolConfig {
                    name: From::from(protocol_name_2),
                    max_request_size: 1024,
                    max_response_size: 1024 * 1024,
                    request_timeout: Duration::from_secs(30),
                    inbound_queue: None,
                },
            ];

            build_swarm(protocol_configs.into_iter()).0
        };

        let (mut swarm_2, mut swarm_2_handler_1, mut swarm_2_handler_2, listen_add_2) = {
            let (tx_1, rx_1) = mpsc::channel(64);
            let (tx_2, rx_2) = mpsc::channel(64);

            let protocol_configs = vec![
                ProtocolConfig {
                    name: From::from(protocol_name_1),
                    max_request_size: 1024,
                    max_response_size: 1024 * 1024,
                    request_timeout: Duration::from_secs(30),
                    inbound_queue: Some(tx_1),
                },
                ProtocolConfig {
                    name: From::from(protocol_name_2),
                    max_request_size: 1024,
                    max_response_size: 1024 * 1024,
                    request_timeout: Duration::from_secs(30),
                    inbound_queue: Some(tx_2),
                },
            ];

            let (swarm, listen_addr) = build_swarm(protocol_configs.into_iter());

            (swarm, rx_1, rx_2, listen_addr)
        };

        // Ask swarm 1 to dial swarm 2. There isn't any discovery mechanism in place in this test,
        // so they wouldn't connect to each other.
        swarm_1.dial_addr(listen_add_2).unwrap();

        // Run swarm 2 in the background, receiving two requests.
        pool.spawner()
            .spawn_obj(
                async move {
                    loop {
                        match swarm_2.select_next_some().await {
                            SwarmEvent::Behaviour(Event::InboundRequest { result, .. }) => {
                                result.unwrap();
                            }
                            _ => {}
                        }
                    }
                }
                .boxed()
                .into(),
            )
            .unwrap();

        // Handle both requests sent by swarm 1 to swarm 2 in the background.
        //
        // Make sure both requests overlap, by answering the first only after receiving the
        // second.
        pool.spawner()
            .spawn_obj(
                async move {
                    let protocol_1_request = swarm_2_handler_1.next().await;
                    let protocol_2_request = swarm_2_handler_2.next().await;

                    protocol_1_request
                        .unwrap()
                        .pending_response
                        .send(OutgoingResponse {
                            result: Ok(b"this is a response".to_vec()),
                            sent_feedback: None,
                        })
                        .unwrap();
                    protocol_2_request
                        .unwrap()
                        .pending_response
                        .send(OutgoingResponse {
                            result: Ok(b"this is a response".to_vec()),

                            sent_feedback: None,
                        })
                        .unwrap();
                }
                .boxed()
                .into(),
            )
            .unwrap();

        // Have swarm 1 send two requests to swarm 2 and await responses.
        pool.run_until(async move {
            let mut response_receivers = None;
            let mut num_responses = 0;

            loop {
                match swarm_1.select_next_some().await {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        let (sender_1, receiver_1) = mpsc::channel::<_>(1);
                        let (sender_2, receiver_2) = mpsc::channel::<_>(1);
                        let ctx = MessageContext {
                            message_type: MessageType::Computation,
                            protocol_id: 0,
                        };
                        swarm_1.behaviour_mut().send_request(
                            &peer_id,
                            protocol_name_1,
                            ctx.clone(),
                            b"this is a request".to_vec(),
                            sender_1,
                            IfDisconnected::ImmediateError,
                        );
                        swarm_1.behaviour_mut().send_request(
                            &peer_id,
                            protocol_name_2,
                            ctx,
                            b"this is a request".to_vec(),
                            sender_2,
                            IfDisconnected::ImmediateError,
                        );
                        assert!(response_receivers.is_none());
                        response_receivers = Some((receiver_1, receiver_2));
                    }
                    SwarmEvent::Behaviour(Event::RequestFinished { result, .. }) => {
                        num_responses += 1;
                        result.unwrap();
                        if num_responses == 2 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let (mut response_receiver_1, mut response_receiver_2) = response_receivers.unwrap();
            let (_, response_1) = response_receiver_1.next().await.unwrap().unwrap();
            let (_, response_2) = response_receiver_2.next().await.unwrap().unwrap();

            assert_eq!(response_1, b"this is a response");
            assert_eq!(response_2, b"this is a response");
        });
    }
}
