use std::fs;

use hyphae_core::rootfs::{ProjectType, create_directory_structure, detect_project_type};
use hyphae_errors::RootfsError;

#[test]
fn detect_javascript_project() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("package.json"), "{}").unwrap();

    let result = detect_project_type(dir.path()).unwrap();
    assert_eq!(result, ProjectType::JavaScript);
}

#[test]
fn detect_rust_project() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

    let result = detect_project_type(dir.path()).unwrap();
    assert_eq!(result, ProjectType::Rust);
}

#[test]
fn detect_ambiguous_project() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("package.json"), "{}").unwrap();
    fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

    let err = detect_project_type(dir.path()).unwrap_err();
    assert!(
        matches!(err, RootfsError::AmbiguousProject { .. }),
        "expected AmbiguousProject, got: {err}"
    );
}

#[test]
fn detect_unrecognized_project() {
    let dir = tempfile::tempdir().unwrap();
    // Empty directory -- no marker files.

    let err = detect_project_type(dir.path()).unwrap_err();
    assert!(
        matches!(err, RootfsError::UnrecognizedProject { .. }),
        "expected UnrecognizedProject, got: {err}"
    );
}

#[test]
fn staging_creates_all_directories() {
    let dir = tempfile::tempdir().unwrap();
    create_directory_structure(dir.path()).unwrap();

    let expected = [
        "dev", "proc", "sys", "sbin", "etc/hyphae", "app", "lib", "tmp",
    ];
    for subdir in &expected {
        let path = dir.path().join(subdir);
        assert!(
            path.is_dir(),
            "expected directory {subdir} to exist at {}",
            path.display()
        );
    }
}

#[test]
fn staging_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    create_directory_structure(dir.path()).unwrap();
    // Calling again must not error.
    create_directory_structure(dir.path()).unwrap();

    let expected = [
        "dev", "proc", "sys", "sbin", "etc/hyphae", "app", "lib", "tmp",
    ];
    for subdir in &expected {
        assert!(dir.path().join(subdir).is_dir());
    }
}
