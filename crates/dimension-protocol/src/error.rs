//! Error types for the dimension wire protocol codec.

/// Errors that can occur during protocol message encoding/decoding.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// Protobuf decode failure.
    #[error("decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    /// Protobuf encode failure.
    #[error("encode error: {0}")]
    Encode(#[from] prost::EncodeError),

    /// Message exceeds the configured maximum size.
    #[error("message too large: {size} bytes exceeds max of {max} bytes")]
    MessageTooLarge {
        /// Actual message size in bytes.
        size: usize,
        /// Configured maximum size in bytes.
        max: usize,
    },

    /// Underlying I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<CodecError> for std::io::Error {
    fn from(err: CodecError) -> Self {
        match err {
            CodecError::Io(io_err) => io_err,
            other => std::io::Error::other(other),
        }
    }
}
