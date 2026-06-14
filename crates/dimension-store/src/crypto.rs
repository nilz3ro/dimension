/// Generate a new API key. Returns (full_key, prefix).
///
/// The full key is a `dim_sk_` prefixed 71-char string (7 + 64 hex chars).
/// The prefix is the first 15 chars (`dim_sk_` + 8 hex chars) — safe to log/display.
/// The full key is returned ONCE and never stored; only its SHA-256 hash is persisted.
pub fn generate_api_key() -> (String, String) {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS entropy unavailable");
    let raw = hex::encode(bytes);
    let full = format!("dim_sk_{}", raw); // 7 + 64 = 71 chars total
    let prefix = full[..15].to_string(); // "dim_sk_" + 8 hex chars
    (full, prefix)
}

/// Compute the SHA-256 hash of an API key (hex-encoded).
pub fn hash_api_key(key: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(key.as_bytes()))
}
