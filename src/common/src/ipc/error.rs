use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("invalid stats segment data: {0}")]
    StatsFormat(String),
}

#[derive(Debug, Error)]
pub enum CtlError {
    #[error("connection failed: {0}")]
    Connect(std::io::Error),
    #[error("request failed: {0}")]
    Request(String),
    #[error("backend returned error: {code} {message}")]
    Backend { code: i32, message: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}
