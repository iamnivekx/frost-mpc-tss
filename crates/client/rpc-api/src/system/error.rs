use crate::RpcError;
use futures::channel::oneshot::Canceled;

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
const BASE_ERROR: i32 = 1000;

impl From<Error> for RpcError {
    fn from(e: Error) -> Self {
        match e {
            Error::Terminated(msg) => RpcError::owned(
                BASE_ERROR + 2,
                format!("Computation terminated with err: {}", msg),
                Some(msg),
            ),
            Error::Canceled(e) => RpcError::owned(
                BASE_ERROR,
                format!("Computation canceled with err: {}", e),
                Some(e.to_string()),
            ),
        }
    }
}
