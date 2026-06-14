//! Content-addressing via SHA-256 tree hashing of source directories.
//!
//! [`hash_source_directory`] walks a project directory, skips common
//! dependency/build artifact directories listed in [`SKIP_DIRS`], sorts
//! entries by relative path for determinism, and produces a single hex-encoded
//! SHA-256 digest covering every file's relative path and contents.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use hyphae_errors::RegistryError;

/// Directories to skip when hashing source files.
///
/// These are dependency/build artifact directories that should not
/// contribute to the content hash.
pub const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    ".pytest_cache",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".output",
];

/// Returns `true` if the entry should be included in the hash walk.
///
/// Directories whose name appears in [`SKIP_DIRS`] are excluded (along with
/// their entire subtree, thanks to `filter_entry` semantics).
fn should_include(entry: &walkdir::DirEntry) -> bool {
    if entry.file_type().is_dir() {
        let name = entry.file_name().to_str().unwrap_or("");
        return !SKIP_DIRS.contains(&name);
    }
    true
}

/// Produce a deterministic SHA-256 digest of a source directory.
///
/// The algorithm:
/// 1. Walk `dir`, skipping directories in [`SKIP_DIRS`].
/// 2. Collect only regular files.
/// 3. Sort by relative path (UTF-8 lossy) for deterministic ordering.
/// 4. For each file, feed the relative path bytes then the file contents
///    into a single [`Sha256`] hasher (in 8 KiB chunks).
/// 5. Return the hex-encoded final digest.
///
/// An empty directory (no files after filtering) produces a valid hash of
/// the empty input.
pub fn hash_source_directory(dir: &Path) -> Result<String, RegistryError> {
    let map_err = |e: std::io::Error, p: &Path| RegistryError::HashingFailed {
        path: p.to_path_buf(),
        message: e.to_string(),
    };

    // Collect file entries, skipping SKIP_DIRS subtrees.
    let mut files: Vec<_> = WalkDir::new(dir)
        .into_iter()
        .filter_entry(should_include)
        .filter_map(|entry| {
            entry
                .map_err(|e| RegistryError::HashingFailed {
                    path: dir.to_path_buf(),
                    message: e.to_string(),
                })
                .ok()
        })
        .filter(|e| e.file_type().is_file())
        .collect();

    // Sort by relative path for deterministic ordering.
    files.sort_by(|a, b| {
        let rel_a = a.path().strip_prefix(dir).unwrap_or(a.path());
        let rel_b = b.path().strip_prefix(dir).unwrap_or(b.path());
        rel_a.cmp(rel_b)
    });

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];

    for entry in &files {
        let rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy();

        // Hash the relative path.
        hasher.update(rel.as_bytes());

        // Hash the file contents in chunks.
        let mut file =
            std::fs::File::open(entry.path()).map_err(|e| map_err(e, entry.path()))?;
        loop {
            let n = file.read(&mut buf).map_err(|e| map_err(e, entry.path()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }

    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_hash_determinism() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        fs::write(dir.path().join("b.txt"), "world").unwrap();

        let h1 = hash_source_directory(dir.path()).unwrap();
        let h2 = hash_source_directory(dir.path()).unwrap();
        assert_eq!(h1, h2, "same directory should produce same hash");
        assert_eq!(h1.len(), 64, "SHA-256 hex should be 64 chars");
    }

    #[test]
    fn test_hash_changes_on_content_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let h1 = hash_source_directory(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "goodbye").unwrap();

        let h2 = hash_source_directory(dir.path()).unwrap();
        assert_ne!(h1, h2, "different content should produce different hash");
    }

    #[test]
    fn test_hash_skips_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.js"), "console.log('hi')").unwrap();

        let h_without = hash_source_directory(dir.path()).unwrap();

        // Add a node_modules directory with files.
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        fs::write(nm.join("dep.js"), "module.exports = {}").unwrap();

        let h_with = hash_source_directory(dir.path()).unwrap();
        assert_eq!(
            h_without, h_with,
            "node_modules should be skipped during hashing"
        );
    }

    #[test]
    fn test_hash_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let h = hash_source_directory(dir.path()).unwrap();
        assert_eq!(h.len(), 64, "empty dir should produce valid hex hash");
    }
}
