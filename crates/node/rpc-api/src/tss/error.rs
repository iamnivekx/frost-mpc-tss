use crate::{RpcError, RpcErrorCode};
use futures::channel::oneshot::Canceled;
// use std::fmt::Display;

/// TSS RPC errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// DagCbor error.
    #[error("computation finished successfully but resulted an unexpected output: {}", .0)]
    DagCborError(String),
    #[error("computation terminated with err: {}", .0)]
    Terminated(String),
    #[error("computation canceled with err: {}", .0)]
    Canceled(Canceled),
}

impl From<Canceled> for Error {
    fn from(e: Canceled) -> Self {
        Self::Canceled(e)
    }
}

impl From<String> for Error {
    fn from(e: String) -> Self {
        Self::Terminated(e)
    }
}

/// Base code for all tss errors.
const BASE_ERROR: i64 = 1000;

impl From<Error> for RpcError {
    fn from(e: Error) -> Self {
        match e {
            Error::DagCborError(e) => RpcError {
                code: RpcErrorCode::ServerError(BASE_ERROR + 1),
                message: format!("Resulted an unexpected output Error: {}", e),
                data: None,
            },
            Error::Terminated(e) => RpcError {
                code: RpcErrorCode::ServerError(BASE_ERROR + 2),
                message: format!("Computation terminated with err: {}", e),
                data: Some(e.to_string().into()),
            },
            Error::Canceled(e) => RpcError {
                code: RpcErrorCode::ServerError(BASE_ERROR),
                message: format!("Computation canceled with err: {}", e),
                data: Some(e.to_string().into()),
            },
        }
    }
}
