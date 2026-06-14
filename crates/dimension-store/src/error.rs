use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("[E901] session not found: {id}")]
    SessionNotFound { id: uuid::Uuid },

    #[error("[E902] database connection error: {0}")]
    Connection(String),

    #[error("[E903] database query failed: {0}")]
    Query(String),

    #[error("[E904] invalid cursor: {reason}")]
    InvalidCursor { reason: String },

    #[error("[E905] user not found: {id}")]
    UserNotFound { id: uuid::Uuid },

    #[error("[E906] API key not found or invalid")]
    KeyNotFound,

    #[error("[E907] cannot demote/delete last admin")]
    LastAdminDemotion,

    #[error("[E908] duplicate entry: {0}")]
    Duplicate(String),

    #[error("[E909] quota exceeded: {resource} limit is {limit}, current count is {current}")]
    QuotaExceeded { resource: String, limit: i64, current: i64 },

    #[error("[E910] forbidden: {0}")]
    Forbidden(String),

    #[error("[E911] error: {0}")]
    Other(String),

    #[error("[E912] conflict: {0}")]
    Conflict(String),

    #[error("[E913] volume not found: {id}")]
    VolumeNotFound { id: uuid::Uuid },

    #[error("[E914] artifact not found: {id}")]
    ArtifactNotFound { id: uuid::Uuid },

    #[error("[E915] object storage error: {0}")]
    ObjectStorage(String),

    #[error("[E916] named volume not found: {id}")]
    NamedVolumeNotFound { id: uuid::Uuid },
}

/// Type alias for backward-compatible re-export.
pub type DimensionStoreError = StoreError;

impl From<sqlx_core::Error> for StoreError {
    fn from(e: sqlx_core::Error) -> Self {
        match e {
            sqlx_core::Error::RowNotFound => StoreError::Query("row not found".to_string()),
            _ => StoreError::Query(e.to_string()),
        }
    }
}
