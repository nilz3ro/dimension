//! Credential store for the dimension CLI.
//!
//! Reads and writes `~/.dimension/credentials` — a simple key-value file that
//! stores the API key used for gateway authentication.  When the `--token` CLI
//! flag is omitted, the CLI transparently loads the stored credential.
//!
//! File format (one key per line):
//! ```text
//! api_key = dk_live_abc123
//! ```

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};

// ── Paths ─────────────────────────────────────────────────────────────────────

/// Return `~/.dimension`.
fn dimension_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".dimension"))
}

/// Return `~/.dimension/credentials`.
fn credentials_path() -> Result<PathBuf> {
    Ok(dimension_dir()?.join("credentials"))
}

// ── Read / Write ──────────────────────────────────────────────────────────────

/// Read the stored API key from `~/.dimension/credentials`.
///
/// Returns `Ok(None)` if the file does not exist or contains no `api_key` line.
pub fn read_api_key() -> Result<Option<String>> {
    read_api_key_from(credentials_path()?)
}

/// Implementation that accepts an arbitrary path — used by tests.
pub(crate) fn read_api_key_from(path: PathBuf) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_api_key(&content))
}

/// Write an API key to `~/.dimension/credentials`, creating the directory if
/// needed.  The file is created with `0600` permissions on Unix.
pub fn write_api_key(key: &str) -> Result<()> {
    write_api_key_to(credentials_path()?, key)
}

/// Implementation that accepts an arbitrary path — used by tests.
pub(crate) fn write_api_key_to(path: PathBuf, key: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content = format!("api_key = {key}\n");
    fs::write(&path, &content)
        .with_context(|| format!("failed to write {}", path.display()))?;

    // Best-effort: restrict permissions on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        let _ = fs::set_permissions(&path, perms);
    }

    Ok(())
}

// ── Token resolution ──────────────────────────────────────────────────────────

/// Resolve the API token to use.
///
/// Priority:
/// 1. Explicit CLI `--token` / `DIMENSION_TOKEN` value.
/// 2. Stored credential from `~/.dimension/credentials`.
///
/// Returns a helpful error if neither source is available.
pub fn resolve_token(cli_token: Option<String>) -> Result<String> {
    if let Some(t) = cli_token
        && !t.is_empty()
    {
        return Ok(t);
    }
    if let Some(stored) = read_api_key()? {
        return Ok(stored);
    }
    anyhow::bail!(
        "No API token found.\n\n\
         Provide one of:\n  \
           --token <TOKEN>\n  \
           DIMENSION_TOKEN env var\n  \
           dimension login   (stores credentials in ~/.dimension/credentials)\n"
    )
}

// ── Interactive prompt ────────────────────────────────────────────────────────

/// Prompt the user for an API key on stdin.
pub fn prompt_api_key() -> Result<String> {
    eprint!("API key: ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .context("failed to read API key from stdin")?;
    let key = line.trim().to_string();
    if key.is_empty() {
        anyhow::bail!("API key cannot be empty");
    }
    Ok(key)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse the `api_key` value from the credentials file content.
fn parse_api_key(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("api_key") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_api_key_basic() {
        assert_eq!(
            parse_api_key("api_key = dk_live_abc123\n"),
            Some("dk_live_abc123".to_string())
        );
    }

    #[test]
    fn parse_api_key_with_comments_and_blanks() {
        let content = "# Dimension credentials\n\napi_key = my_key_42\n";
        assert_eq!(parse_api_key(content), Some("my_key_42".to_string()));
    }

    #[test]
    fn parse_api_key_missing() {
        assert_eq!(parse_api_key(""), None);
        assert_eq!(parse_api_key("# nothing here\n"), None);
    }

    #[test]
    fn parse_api_key_no_equals() {
        assert_eq!(parse_api_key("api_key dk_oops\n"), None);
    }

    #[test]
    fn write_then_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("credentials");

        write_api_key_to(path.clone(), "test_key_123").unwrap();
        let read = read_api_key_from(path).unwrap();
        assert_eq!(read, Some("test_key_123".to_string()));
    }

    #[test]
    fn read_missing_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent");
        let read = read_api_key_from(path).unwrap();
        assert_eq!(read, None);
    }

    #[test]
    fn write_creates_parent_directories() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a").join("b").join("credentials");
        write_api_key_to(path.clone(), "nested_key").unwrap();
        assert!(path.exists());
    }

    #[test]
    fn resolve_prefers_cli_token() {
        // Can't test the file-fallback path without mocking, but we can verify
        // that an explicit token takes priority.
        let token = super::resolve_token(Some("explicit".to_string())).unwrap();
        assert_eq!(token, "explicit");
    }

    #[test]
    fn resolve_rejects_empty_cli_token_falls_through() {
        // Empty string should not count as a valid CLI token.
        // Since there's no credentials file in a test env, this should error
        // with a helpful message.
        let err = super::resolve_token(Some(String::new())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("No API token found"), "got: {msg}");
    }
}
