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
    #[error("no embedded table key matches key slot {index}")]
    UnknownKey { index: usize },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
