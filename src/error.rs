#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid RPF9 header at {at:#x}: {reason}")]
    InvalidHeader { at: usize, reason: &'static str },
    #[error("invalid archive layout: {reason}")]
    InvalidLayout { reason: &'static str },
    #[error("zlib stream at {at:#x}: {message}")]
    Zlib { at: usize, message: String },
    #[error("inflated size mismatch at {at:#x}: expected {expected} bytes, got {actual}")]
    InflatedSize {
        at: usize,
        expected: u64,
        actual: u64,
    },
    #[error("entry is too large for memory: {size} bytes")]
    TooLarge { size: u64 },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
