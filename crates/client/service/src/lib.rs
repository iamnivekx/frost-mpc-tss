mod coordination;
mod echo;
mod error;
mod execution;
mod handler;
mod negotiation;
mod network_proxy;
mod peerset;
mod service;
mod storage;
mod worker;

pub use error::*;
use futures::channel::{mpsc, oneshot};
pub use handler::*;
use mpc_network::{request_responses, NetworkService, RoomId};
pub use peerset::*;
pub use service::*;
pub use storage::*;
pub use worker::*;

/// Create a new tss [`Worker`] and [`Service`].
///
/// See the struct documentation of each for more details.
pub fn new_worker_and_service<T>(
    network_service: NetworkService,
    rooms: impl Iterator<Item = (RoomId, mpsc::Receiver<request_responses::IncomingRequest>)>,
    agents_factory: T,
    peerset_storage: LocalStorage,
) -> (Worker<T>, Service)
where
    T: ProtocolAgentFactory + Send + Unpin,
{
    let (to_worker, from_service) = mpsc::channel(0);

    let worker = Worker::new(
        from_service,
        network_service,
        rooms,
        agents_factory,
        peerset_storage,
    );
    let service = Service::new(to_worker);

    (worker, service)
}

/// Message send from the [`Service`] to the [`Worker`].
pub enum ServicetoWorkerMsg {
    KeySign(
        u16,
        RoomId,
        Vec<u8>,
        oneshot::Sender<anyhow::Result<Vec<u8>>>,
    ),
    /// See [`Service::keygen`].
    KeyGen(
        u16,
        u16,
        RoomId,
        Vec<u8>,
        oneshot::Sender<anyhow::Result<Vec<u8>>>,
    ),
}
