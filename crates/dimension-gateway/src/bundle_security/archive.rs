//! Archive validation and extraction with security guards.
//!
//! # WARNING
//! The declared size check is a fast pre-flight guard. For production hardening
//! against crafted headers with mismatched declared vs actual sizes, consider
//! adding a counting writer during extraction. See RESEARCH.md Pitfall 2.

use std::path::{Component, Path, PathBuf};

/// Errors produced during archive validation.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("path traversal detected: {path}")]
    PathTraversal { path: PathBuf },

    #[error("absolute path rejected: {path}")]
    AbsolutePath { path: PathBuf },

    #[error("symlinks not allowed: {path}")]
    SymlinkRejected { path: PathBuf },

    #[error("entry exceeds 4GB limit: {path} ({size} bytes)")]
    EntryTooLarge { path: PathBuf, size: u64 },

    #[error("total extracted size exceeds 8GB limit ({total} bytes)")]
    TotalSizeExceeded { total: u64 },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Maximum allowed size for a single archive entry (4 GiB).
const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Maximum allowed total extracted size (8 GiB).
const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Validate and extract a tar archive to `dest`.
///
/// Performs a two-pass approach:
/// 1. **Validation pass** — iterates entries checking for security violations
///    without writing anything to disk.
/// 2. **Extraction pass** — only runs if validation succeeds; unpacks cleanly.
///
/// # Errors
/// Returns [`ArchiveError`] on any security violation or I/O failure.
pub fn validate_and_extract(tar_path: &Path, dest: &Path) -> Result<(), ArchiveError> {
    // --- First pass: validate ---
    let file = std::fs::File::open(tar_path)?;
    let mut archive = tar::Archive::new(file);
    let mut total_bytes: u64 = 0;

    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?.into_owned();

        // Reject absolute paths
        if path.is_absolute() {
            return Err(ArchiveError::AbsolutePath { path });
        }

        // Reject path traversal (..)
        for component in path.components() {
            if component == Component::ParentDir {
                return Err(ArchiveError::PathTraversal { path: path.clone() });
            }
        }

        // Reject symlinks and hard links
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(ArchiveError::SymlinkRejected { path });
        }

        // Check per-entry declared size
        let size = entry.header().size()?;
        if size > MAX_ENTRY_BYTES {
            return Err(ArchiveError::EntryTooLarge { path, size });
        }

        // Accumulate total and check bomb limit
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(ArchiveError::TotalSizeExceeded { total: total_bytes });
        }
    }

    // --- Second pass: extract (only if validation passed) ---
    let file = std::fs::File::open(tar_path)?;
    let mut archive = tar::Archive::new(file);
    archive.unpack(dest)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{tempdir, NamedTempFile};

    /// Type of entry to add to the test archive.
    enum EntryKind {
        Regular,
        Symlink { target: PathBuf },
        HardLink { target: PathBuf },
        Dir,
    }

    struct TestEntry {
        path: String,
        content: Vec<u8>,
        kind: EntryKind,
        /// Override the declared header size (for zip-bomb / oversized tests).
        declared_size_override: Option<u64>,
    }

    impl TestEntry {
        fn regular(path: &str, content: &[u8]) -> Self {
            Self {
                path: path.into(),
                content: content.to_vec(),
                kind: EntryKind::Regular,
                declared_size_override: None,
            }
        }

        fn with_declared_size(path: &str, declared: u64) -> Self {
            Self {
                path: path.into(),
                content: vec![0u8; 4], // tiny actual content
                kind: EntryKind::Regular,
                declared_size_override: Some(declared),
            }
        }
    }

    /// Write a raw GNU tar header with the given path string embedded directly
    /// in the name field (bypassing `set_path` validation). This allows tests
    /// to construct archives with malicious paths that the `tar` crate would
    /// normally refuse to build via the safe API.
    fn write_raw_header(
        writer: &mut impl Write,
        path_str: &str,
        entry_type: tar::EntryType,
        size: u64,
        link_name: &str,
    ) {
        // GNU tar header is 512 bytes
        let mut block = [0u8; 512];

        // name field: bytes 0..100
        let name_bytes = path_str.as_bytes();
        let name_len = name_bytes.len().min(100);
        block[..name_len].copy_from_slice(&name_bytes[..name_len]);

        // mode field: bytes 100..108
        let mode = b"0000644\0";
        block[100..108].copy_from_slice(mode);

        // uid/gid: bytes 108..116 and 116..124
        block[108..116].copy_from_slice(b"0000000\0");
        block[116..124].copy_from_slice(b"0000000\0");

        // size: bytes 124..136 (octal, 11 digits + null)
        let size_str = format!("{:011o}\0", size);
        block[124..136].copy_from_slice(size_str.as_bytes());

        // mtime: bytes 136..148
        block[136..148].copy_from_slice(b"00000000000\0");

        // typeflag: byte 156
        block[156] = match entry_type {
            tar::EntryType::Regular => b'0',
            tar::EntryType::Link => b'1',
            tar::EntryType::Symlink => b'2',
            tar::EntryType::Directory => b'5',
            _ => b'0',
        };

        // link name: bytes 157..257
        if !link_name.is_empty() {
            let link_bytes = link_name.as_bytes();
            let link_len = link_bytes.len().min(100);
            block[157..157 + link_len].copy_from_slice(&link_bytes[..link_len]);
        }

        // magic + version (POSIX ustar)
        block[257..263].copy_from_slice(b"ustar ");
        block[263..265].copy_from_slice(b" \0");

        // Compute checksum (sum of all bytes with checksum field treated as spaces)
        for b in block[148..156].iter_mut() {
            *b = b' ';
        }
        let checksum: u32 = block.iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", checksum);
        block[148..156].copy_from_slice(cksum_str.as_bytes());

        writer.write_all(&block).expect("write header block");
    }

    fn create_test_tar(entries: &[TestEntry]) -> NamedTempFile {
        let mut tmp = NamedTempFile::new().expect("tmpfile");
        {
            // We use raw header writing for entries with dangerous paths or overridden sizes,
            // and the safe tar::Builder API for normal entries.
            let file = tmp.as_file_mut();
            for entry in entries {
                let needs_raw = entry.path.starts_with('/')
                    || entry.path.contains("..")
                    || entry.declared_size_override.is_some();

                if needs_raw || matches!(entry.kind, EntryKind::Symlink { .. } | EntryKind::HardLink { .. }) {
                    let (etype, link_name) = match &entry.kind {
                        EntryKind::Symlink { target } => {
                            (tar::EntryType::Symlink, target.to_string_lossy().into_owned())
                        }
                        EntryKind::HardLink { target } => {
                            (tar::EntryType::Link, target.to_string_lossy().into_owned())
                        }
                        EntryKind::Dir => (tar::EntryType::Directory, String::new()),
                        _ => (tar::EntryType::Regular, String::new()),
                    };
                    let size = entry
                        .declared_size_override
                        .unwrap_or(entry.content.len() as u64);
                    write_raw_header(file, &entry.path, etype, size, &link_name);
                    // Write content padded to 512-byte block
                    if !entry.content.is_empty() {
                        file.write_all(&entry.content).expect("write content");
                        let pad = (512 - (entry.content.len() % 512)) % 512;
                        if pad > 0 {
                            let zeros = vec![0u8; pad];
                            file.write_all(&zeros).expect("write padding");
                        }
                    }
                } else {
                    // Safe path — use tar::Builder for a single entry
                    let mut builder = tar::Builder::new(std::io::Cursor::new(Vec::new()));
                    let mut header = tar::Header::new_gnu();
                    header.set_path(&entry.path).expect("set path");
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(entry.content.len() as u64);
                    header.set_cksum();
                    builder
                        .append(&header, entry.content.as_slice())
                        .expect("append");
                    builder.finish().expect("finish");
                    let data = builder.into_inner().expect("inner").into_inner();
                    // Strip the end-of-archive 1024-byte trailer (two null blocks)
                    let stripped = if data.len() > 1024 {
                        &data[..data.len() - 1024]
                    } else {
                        &data
                    };
                    file.write_all(stripped).expect("write entry data");
                }
            }
            // End-of-archive marker: two 512-byte zero blocks
            file.write_all(&[0u8; 1024]).expect("write EOF blocks");
        }
        tmp
    }

    #[test]
    fn valid_tar_extracts_successfully() {
        let tar = create_test_tar(&[
            TestEntry::regular("hello.txt", b"hello world"),
            TestEntry::regular("subdir/foo.txt", b"bar"),
        ]);
        let dest = tempdir().expect("tempdir");

        validate_and_extract(tar.path(), dest.path()).expect("should succeed");

        assert!(dest.path().join("hello.txt").exists(), "hello.txt must exist");
        assert!(
            dest.path().join("subdir/foo.txt").exists(),
            "subdir/foo.txt must exist"
        );
    }

    #[test]
    fn path_traversal_is_rejected() {
        let tar = create_test_tar(&[TestEntry::regular(
            "../../etc/passwd",
            b"evil",
        )]);
        let dest = tempdir().expect("tempdir");

        let err = validate_and_extract(tar.path(), dest.path())
            .expect_err("should reject traversal");
        assert!(
            matches!(err, ArchiveError::PathTraversal { .. }),
            "expected PathTraversal, got: {err}"
        );
    }

    #[test]
    fn absolute_path_is_rejected() {
        let tar = create_test_tar(&[TestEntry::regular("/etc/passwd", b"evil")]);
        let dest = tempdir().expect("tempdir");

        let err = validate_and_extract(tar.path(), dest.path())
            .expect_err("should reject absolute path");
        assert!(
            matches!(err, ArchiveError::AbsolutePath { .. }),
            "expected AbsolutePath, got: {err}"
        );
    }

    #[test]
    fn symlink_is_rejected() {
        let tar = create_test_tar(&[TestEntry {
            path: "link".into(),
            content: vec![],
            kind: EntryKind::Symlink {
                target: PathBuf::from("/etc/passwd"),
            },
            declared_size_override: None,
        }]);
        let dest = tempdir().expect("tempdir");

        let err = validate_and_extract(tar.path(), dest.path())
            .expect_err("should reject symlink");
        assert!(
            matches!(err, ArchiveError::SymlinkRejected { .. }),
            "expected SymlinkRejected, got: {err}"
        );
    }

    #[test]
    fn hard_link_is_rejected() {
        let tar = create_test_tar(&[TestEntry {
            path: "hardlink".into(),
            content: vec![],
            kind: EntryKind::HardLink {
                target: PathBuf::from("existing_file.txt"),
            },
            declared_size_override: None,
        }]);
        let dest = tempdir().expect("tempdir");

        let err = validate_and_extract(tar.path(), dest.path())
            .expect_err("should reject hard link");
        assert!(
            matches!(err, ArchiveError::SymlinkRejected { .. }),
            "expected SymlinkRejected for hard link, got: {err}"
        );
    }

    #[test]
    fn single_entry_exceeding_4gb_is_rejected() {
        // Use declared_size_override to set header size > 4GiB without real data
        let too_large = MAX_ENTRY_BYTES + 1;
        let tar = create_test_tar(&[TestEntry::with_declared_size("bigfile.bin", too_large)]);
        let dest = tempdir().expect("tempdir");

        let err = validate_and_extract(tar.path(), dest.path())
            .expect_err("should reject oversized entry");
        assert!(
            matches!(err, ArchiveError::EntryTooLarge { .. }),
            "expected EntryTooLarge, got: {err}"
        );
    }

    /// Build a tar archive where entries have the given declared size but only
    /// contain the given content. On Linux this is done by writing the content
    /// followed by a seek to pad each entry's block to the declared size, creating
    /// a sparse file so the tar iterator can advance between headers without
    /// reading gigabytes of data off disk.
    fn create_sparse_tar_with_entries(entries: &[(u64, &[u8])]) -> NamedTempFile {
        use std::io::Seek;
        let mut tmp = NamedTempFile::new().expect("tmpfile");
        {
            let file = tmp.as_file_mut();
            for (i, (declared_size, content)) in entries.iter().enumerate() {
                let path = format!("part{i}.bin");
                write_raw_header(file, &path, tar::EntryType::Regular, *declared_size, "");

                // Write actual content
                file.write_all(content).expect("write content");

                // Seek forward to make the file sparse: position at end of declared data
                // (rounded up to 512-byte block boundary), so the tar iterator can skip it
                let content_len = content.len() as u64;
                let padded_declared = (declared_size + 511) & !511;
                let already_written = content_len;
                let remaining = padded_declared.saturating_sub(already_written);
                if remaining > 0 {
                    // Seek forward (creates a hole on supporting filesystems)
                    file.seek(std::io::SeekFrom::Current(remaining as i64))
                        .expect("seek");
                    // Write one byte at the new position to extend the file
                    file.write_all(&[0u8]).expect("extend file");
                    // Seek back one byte so next entry starts correctly
                    file.seek(std::io::SeekFrom::Current(-1))
                        .expect("seek back");
                }
            }
            // End-of-archive marker
            file.write_all(&[0u8; 1024]).expect("write EOF blocks");
        }
        tmp
    }

    #[test]
    fn cumulative_size_exceeding_8gb_is_rejected() {
        // Two entries each 4.5 GiB in declared header size but under the per-entry 4GiB limit?
        // No — we need each entry < 4GiB but total > 8GiB.
        // Use 3 entries of 3 GiB each → total 9 GiB, each under the per-entry limit.
        // The sparse file approach lets the tar iterator skip without reading 3GB.
        let three_gb = 3u64 * 1024 * 1024 * 1024;
        let tar = create_sparse_tar_with_entries(&[
            (three_gb, b""),
            (three_gb, b""),
            (three_gb, b""),
        ]);
        let dest = tempdir().expect("tempdir");

        let err = validate_and_extract(tar.path(), dest.path())
            .expect_err("should reject zip bomb");
        assert!(
            matches!(err, ArchiveError::TotalSizeExceeded { .. }),
            "expected TotalSizeExceeded, got: {err}"
        );
    }

    #[test]
    fn extracted_files_exist_in_destination() {
        let tar = create_test_tar(&[
            TestEntry::regular("a.txt", b"content-a"),
            TestEntry::regular("nested/b.txt", b"content-b"),
        ]);
        let dest = tempdir().expect("tempdir");

        validate_and_extract(tar.path(), dest.path()).expect("extraction should succeed");

        let a_content = std::fs::read(dest.path().join("a.txt")).expect("a.txt");
        assert_eq!(a_content, b"content-a");

        let b_content = std::fs::read(dest.path().join("nested/b.txt")).expect("nested/b.txt");
        assert_eq!(b_content, b"content-b");
    }
}
