pub mod client;
pub mod server;
pub mod system;
pub mod tss;

pub use client::new_client;
pub use jsonrpsee::types::ErrorObjectOwned as RpcError;
pub type RpcResult<T> = Result<T, RpcError>;
pub use jsonrpsee::RpcModule as JsonRpcModule;
pub use system::{SystemApiClient, SystemApiServer};
pub use tss::{error::Error as TssError, TssApiClient, TssApiServer};
