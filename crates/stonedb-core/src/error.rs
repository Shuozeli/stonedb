//! StoneDB Core Error Types

use std::fmt;

/// Result type for StoneDB operations
pub type Result<T> = std::result::Result<T, Error>;

/// StoneDB Core Errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Key not found
    NotFound,
    /// Invalid key format
    InvalidKey(String),
    /// Invalid value format
    InvalidValue(String),
    /// Internal error
    Internal(String),
    /// Iterator is not valid
    InvalidIterator,
    /// Sequence number overflow
    SequenceOverflow,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => write!(f, "key not found"),
            Error::InvalidKey(msg) => write!(f, "invalid key: {}", msg),
            Error::InvalidValue(msg) => write!(f, "invalid value: {}", msg),
            Error::Internal(msg) => write!(f, "internal error: {}", msg),
            Error::InvalidIterator => write!(f, "iterator is not valid"),
            Error::SequenceOverflow => write!(f, "sequence number overflow"),
        }
    }
}

impl std::error::Error for Error {}
