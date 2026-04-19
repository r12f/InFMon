use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("failed to open stats segment: {0}")]
    StatsOpen(std::io::Error),
    #[error("stats segment busy (another reader holds the lock)")]
    StatsSegmentBusy,
    #[error("stats segment I/O error: {0}")]
    StatsIo(std::io::Error),
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
