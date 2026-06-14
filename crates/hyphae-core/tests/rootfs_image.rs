use std::fs;
use std::io::Write;
use std::path::Path;

use hyphae_core::rootfs::image::calculate_image_sizing;

const MIB: u64 = 1024 * 1024;
const BLOCK_SIZE: u64 = 4096;

/// Helper: create `count` files of `size` bytes each in `dir`.
fn create_files(dir: &Path, count: usize, size: u64) {
    for i in 0..count {
        let path = dir.join(format!("file_{i}.dat"));
        let mut f = fs::File::create(&path).unwrap();
        // Write `size` bytes of zeros
        let mut remaining = size;
        let buf = vec![0u8; 8192];
        while remaining > 0 {
            let to_write = remaining.min(buf.len() as u64) as usize;
            f.write_all(&buf[..to_write]).unwrap();
            remaining -= to_write as u64;
        }
    }
}

/// Helper: create `count` subdirectories in `dir`.
fn create_subdirs(dir: &Path, count: usize) {
    for i in 0..count {
        fs::create_dir_all(dir.join(format!("subdir_{i}"))).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Image sizing tests
// ---------------------------------------------------------------------------

#[test]
fn test_sizing_small_project_hits_floor() {
    // 5 files totaling ~100 KiB + 3 subdirectories -> floor of 16 MiB should win
    let dir = tempfile::tempdir().unwrap();
    create_files(dir.path(), 5, 20 * 1024); // 5 * 20 KiB = 100 KiB
    create_subdirs(dir.path(), 3);

    let sizing = calculate_image_sizing(dir.path(), None).unwrap();

    assert_eq!(sizing.total_size, 16 * MIB, "small project should hit 16 MiB floor");
    assert!(sizing.content_size > 0, "content_size should be non-zero");
}

#[test]
fn test_sizing_large_project_uses_headroom() {
    // 4 files of 25 MiB each = 100 MiB -> headroom = 120 MiB (rounded to 4K)
    let dir = tempfile::tempdir().unwrap();
    create_files(dir.path(), 4, 25 * MIB);

    let sizing = calculate_image_sizing(dir.path(), None).unwrap();

    // 100 MiB * 1.20 = 120 MiB = 125829120 bytes, which is already 4K-aligned
    let expected_min = (100.0 * MIB as f64 * 1.20) as u64;
    assert!(
        sizing.total_size >= expected_min,
        "total_size {} should be >= {} (100 MiB * 1.20)",
        sizing.total_size,
        expected_min
    );
    assert!(
        sizing.total_size > 16 * MIB,
        "total_size should exceed 16 MiB floor"
    );
    // Must be block-aligned
    assert_eq!(
        sizing.total_size % BLOCK_SIZE,
        0,
        "total_size must be 4096-aligned"
    );
}

#[test]
fn test_sizing_override() {
    // size_override of 200 MiB should bypass calculation
    let dir = tempfile::tempdir().unwrap();
    create_files(dir.path(), 2, 1024); // small content

    let sizing = calculate_image_sizing(dir.path(), Some(200 * MIB)).unwrap();

    assert_eq!(sizing.total_size, 200 * MIB, "override should set total_size directly");
}

#[test]
fn test_sizing_empty_dir() {
    let dir = tempfile::tempdir().unwrap();

    let sizing = calculate_image_sizing(dir.path(), None).unwrap();

    assert_eq!(sizing.total_size, 16 * MIB, "empty dir should hit 16 MiB floor");
    assert_eq!(sizing.content_size, 0, "empty dir has zero content_size");
}

#[test]
fn test_sizing_inode_count() {
    // Create 5 files and 3 subdirectories.
    // WalkDir includes the root dir itself, so entries = 1 (root) + 5 (files) + 3 (dirs) = 9
    // But file_count in our contract = files + directories (excluding root? or including?)
    // The plan says: "file_count = 8 (5 files + 3 dirs)" for a dir with 5 files and 3 dirs
    // So file_count = entries seen by walkdir minus the root = files + dirs
    // inode_count = file_count + 10
    let dir = tempfile::tempdir().unwrap();
    create_files(dir.path(), 5, 1024);
    create_subdirs(dir.path(), 3);

    let sizing = calculate_image_sizing(dir.path(), None).unwrap();

    // file_count = 5 files + 3 dirs = 8
    assert_eq!(sizing.file_count, 8, "file_count should be 5 files + 3 dirs");
    // inode_count = file_count + 10
    assert_eq!(sizing.inode_count, 18, "inode_count should be file_count + 10 overhead");
}

#[test]
fn test_sizing_block_alignment() {
    // Create a project that would result in non-block-aligned content
    // e.g., 14 MiB content -> 14 * 1.20 = 16.8 MiB -> must round up to block boundary
    let dir = tempfile::tempdir().unwrap();
    create_files(dir.path(), 1, 14 * MIB);

    let sizing = calculate_image_sizing(dir.path(), None).unwrap();

    assert_eq!(
        sizing.total_size % BLOCK_SIZE,
        0,
        "total_size {} must be a multiple of 4096",
        sizing.total_size
    );
    // 14 MiB * 1.20 = 16.8 MiB = 17616076.8 -> ceil to 4K = some value > 16 MiB
    assert!(
        sizing.total_size >= 16 * MIB,
        "total_size should be at least 16 MiB floor"
    );
}

// ---------------------------------------------------------------------------
// build_rootfs pipeline test (requires mkfs.ext4 + real project -- ignored)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_build_rootfs_js_project() {
    use hyphae_core::rootfs::image::{build_rootfs, BuildConfig};

    let project_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    let output_path = output_dir.path().join("rootfs.ext4");

    // Create a minimal JS project
    fs::write(
        project_dir.path().join("package.json"),
        r#"{"scripts":{"start":"node index.js"}}"#,
    )
    .unwrap();
    fs::write(
        project_dir.path().join("index.js"),
        "console.log('hello');",
    )
    .unwrap();

    let config = BuildConfig {
        project_dir: project_dir.path().to_path_buf(),
        output_path: output_path.clone(),
        size_override: None,
        embed_dimension_agent: false,
        binary_path: None,
        entrypoint: None,
        docker_image: None,
    };

    let result = build_rootfs(&config).unwrap();

    assert_eq!(result.image_path, output_path);
    assert!(result.image_size >= 16 * MIB);
    assert!(result.content_size > 0);
    assert!(result.inode_count > 0);
    assert!(result.inode_usage > 0);
}
