use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::StoreError;

/// The data encoded inside an opaque cursor string.
#[derive(Debug, Serialize, Deserialize)]
pub struct CursorPayload {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

/// Encode a (created_at, id) pair into an opaque base64 cursor string.
pub fn encode_cursor(created_at: DateTime<Utc>, id: Uuid) -> String {
    let payload = CursorPayload { created_at, id };
    let json = serde_json::to_string(&payload).expect("cursor serialization is infallible");
    URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// Decode an opaque cursor string back to its (created_at, id) pair.
pub fn decode_cursor(cursor_str: &str) -> Result<CursorPayload, StoreError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor_str.as_bytes())
        .map_err(|e| StoreError::InvalidCursor {
            reason: format!("base64 decode failed: {e}"),
        })?;

    let s = std::str::from_utf8(&bytes).map_err(|e| StoreError::InvalidCursor {
        reason: format!("invalid UTF-8 in cursor: {e}"),
    })?;

    serde_json::from_str(s).map_err(|e| StoreError::InvalidCursor {
        reason: format!("JSON decode failed: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_roundtrip() {
        let ts = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let id = Uuid::new_v4();
        let encoded = encode_cursor(ts, id);
        let decoded = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.created_at, ts);
    }

    #[test]
    fn test_invalid_base64() {
        let err = decode_cursor("not-valid-base64!!!").unwrap_err();
        assert!(matches!(err, StoreError::InvalidCursor { .. }));
    }

    #[test]
    fn test_invalid_json() {
        let encoded = URL_SAFE_NO_PAD.encode(b"not json");
        let err = decode_cursor(&encoded).unwrap_err();
        assert!(matches!(err, StoreError::InvalidCursor { .. }));
    }
}
