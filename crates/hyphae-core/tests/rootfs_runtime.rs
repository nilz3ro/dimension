use std::fs;
use std::os::unix::fs::PermissionsExt;

use hyphae_core::rootfs::{
    build_rust_project, detect_js_entrypoint, detect_rust_binary, prepare_js_runtime,
    prepare_rust_runtime,
};
use hyphae_errors::RootfsError;

// ---------------------------------------------------------------------------
// JS entrypoint detection tests
// ---------------------------------------------------------------------------

#[test]
fn js_entrypoint_from_scripts_start() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{"scripts": {"start": "node server.js"}}"#,
    )
    .unwrap();

    let result = detect_js_entrypoint(dir.path()).unwrap();
    assert_eq!(result, vec!["node", "server.js"]);
}

#[test]
fn js_entrypoint_from_main_field() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{"main": "app.js"}"#,
    )
    .unwrap();

    let result = detect_js_entrypoint(dir.path()).unwrap();
    assert_eq!(result, vec!["node", "app.js"]);
}

#[test]
fn js_entrypoint_from_index_js() {
    let dir = tempfile::tempdir().unwrap();
    // package.json with no scripts.start and no main
    fs::write(dir.path().join("package.json"), r#"{}"#).unwrap();
    // But index.js exists
    fs::write(dir.path().join("index.js"), "console.log('hello')").unwrap();

    let result = detect_js_entrypoint(dir.path()).unwrap();
    assert_eq!(result, vec!["node", "index.js"]);
}

#[test]
fn js_entrypoint_no_entrypoint_error() {
    let dir = tempfile::tempdir().unwrap();
    // package.json with no scripts.start, no main, and no index.js
    fs::write(dir.path().join("package.json"), r#"{}"#).unwrap();

    let err = detect_js_entrypoint(dir.path()).unwrap_err();
    assert!(
        matches!(err, RootfsError::NoEntrypoint { .. }),
        "expected NoEntrypoint, got: {err}"
    );
}

#[test]
fn js_entrypoint_missing_package_json() {
    let dir = tempfile::tempdir().unwrap();
    // No package.json at all

    let err = detect_js_entrypoint(dir.path()).unwrap_err();
    assert!(
        matches!(err, RootfsError::PackageJsonRead(_)),
        "expected PackageJsonRead, got: {err}"
    );
}

#[test]
fn js_entrypoint_malformed_package_json() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("package.json"), "not valid json {{{").unwrap();

    let err = detect_js_entrypoint(dir.path()).unwrap_err();
    assert!(
        matches!(err, RootfsError::PackageJsonParse(_)),
        "expected PackageJsonParse, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Rust binary detection tests
// ---------------------------------------------------------------------------

#[test]
fn rust_detect_single_binary() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "myapp"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "myapp"
path = "src/main.rs"
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    let result = detect_rust_binary(dir.path()).unwrap();
    assert_eq!(result, "myapp");
}

#[test]
fn rust_detect_default_run() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "myapp"
version = "0.1.0"
edition = "2021"
default-run = "server"

[[bin]]
name = "server"
path = "src/server.rs"

[[bin]]
name = "worker"
path = "src/worker.rs"
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/server.rs"), "fn main() {}").unwrap();
    fs::write(dir.path().join("src/worker.rs"), "fn main() {}").unwrap();

    let result = detect_rust_binary(dir.path()).unwrap();
    assert_eq!(result, "server");
}

#[test]
fn rust_detect_multiple_without_default_run() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "myapp"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "server"
path = "src/server.rs"

[[bin]]
name = "worker"
path = "src/worker.rs"
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/server.rs"), "fn main() {}").unwrap();
    fs::write(dir.path().join("src/worker.rs"), "fn main() {}").unwrap();

    let err = detect_rust_binary(dir.path()).unwrap_err();
    assert!(
        matches!(err, RootfsError::MultipleBinaries { .. }),
        "expected MultipleBinaries, got: {err}"
    );
}

#[test]
fn rust_detect_no_binary_target() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "mylib"
version = "0.1.0"
edition = "2021"

[lib]
name = "mylib"
path = "src/lib.rs"
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn hello() {}").unwrap();

    let err = detect_rust_binary(dir.path()).unwrap_err();
    assert!(
        matches!(err, RootfsError::NoBinaryTarget { .. }),
        "expected NoBinaryTarget, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Rust build tests (marked #[ignore] because they compile real code)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn rust_build_project_success() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "testbin"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "testbin"
path = "src/main.rs"
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() { println!(\"hello\"); }",
    )
    .unwrap();

    let binary_path = build_rust_project(dir.path(), "testbin").unwrap();
    assert!(binary_path.exists(), "compiled binary should exist");
    assert!(
        binary_path.ends_with("target/release/testbin"),
        "binary path should end with target/release/testbin, got: {}",
        binary_path.display()
    );
}

#[test]
#[ignore]
fn rust_build_project_failure() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "badbin"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "badbin"
path = "src/main.rs"
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    // Intentionally broken Rust code
    fs::write(dir.path().join("src/main.rs"), "fn main() { BROKEN }").unwrap();

    let err = build_rust_project(dir.path(), "badbin").unwrap_err();
    assert!(
        matches!(err, RootfsError::CargoBuildFailed { .. }),
        "expected CargoBuildFailed, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// JS runtime preparation tests
// ---------------------------------------------------------------------------

#[test]
fn prepare_js_runtime_creates_node_binary_and_copies_project() {
    let staging = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();

    // Create a JS project
    fs::write(
        project.path().join("package.json"),
        r#"{"scripts": {"start": "node server.js"}}"#,
    )
    .unwrap();
    fs::write(project.path().join("server.js"), "console.log('hi')").unwrap();

    let cmd = prepare_js_runtime(staging.path(), project.path()).unwrap();

    // Check Node.js binary was placed
    let node_path = staging.path().join("usr/local/bin/node");
    assert!(node_path.exists(), "node binary should exist in staging");
    let mode = fs::metadata(&node_path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755, "node binary should be executable");

    // Check project files were copied
    assert!(
        staging.path().join("app/package.json").exists(),
        "package.json should be in staging/app/"
    );
    assert!(
        staging.path().join("app/server.js").exists(),
        "server.js should be in staging/app/"
    );

    // Check entrypoint command
    assert_eq!(cmd, vec!["/usr/local/bin/node", "/app/server.js"]);
}

#[test]
fn prepare_js_runtime_with_main_field() {
    let staging = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();

    fs::write(
        project.path().join("package.json"),
        r#"{"main": "lib/index.js"}"#,
    )
    .unwrap();
    fs::create_dir_all(project.path().join("lib")).unwrap();
    fs::write(project.path().join("lib/index.js"), "module.exports = {}").unwrap();

    let cmd = prepare_js_runtime(staging.path(), project.path()).unwrap();
    assert_eq!(cmd, vec!["/usr/local/bin/node", "/app/lib/index.js"]);
}

#[test]
fn prepare_js_runtime_excludes_git_dir() {
    let staging = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();

    fs::write(
        project.path().join("package.json"),
        r#"{"scripts": {"start": "node index.js"}}"#,
    )
    .unwrap();
    fs::write(project.path().join("index.js"), "").unwrap();
    // Create .git directory that should be excluded
    fs::create_dir_all(project.path().join(".git/objects")).unwrap();
    fs::write(project.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();

    prepare_js_runtime(staging.path(), project.path()).unwrap();

    assert!(
        !staging.path().join("app/.git").exists(),
        ".git directory should not be copied to staging"
    );
}

// ---------------------------------------------------------------------------
// Rust runtime preparation tests (marked #[ignore] because they compile code)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn prepare_rust_runtime_builds_and_stages_binary() {
    let staging = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();

    fs::write(
        project.path().join("Cargo.toml"),
        r#"[package]
name = "testapp"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "testapp"
path = "src/main.rs"
"#,
    )
    .unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/main.rs"),
        "fn main() { println!(\"hello from testapp\"); }",
    )
    .unwrap();

    let cmd = prepare_rust_runtime(staging.path(), project.path()).unwrap();

    // Check binary was staged
    let staged_binary = staging.path().join("app/testapp");
    assert!(staged_binary.exists(), "binary should be in staging/app/");
    let mode = fs::metadata(&staged_binary).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755, "binary should be executable");

    // Check entrypoint
    assert_eq!(cmd, vec!["/app/testapp"]);
}
