//! Build script for hyphae-core.
//!
//! Handles two embedded binaries:
//!
//! 1. **hyphae-init** -- Compiles the hyphae-init binary for x86_64-unknown-linux-musl
//!    (on Linux) and places it in OUT_DIR for embedding via `include_bytes!`.
//!
//! 2. **node-binary** -- Copies a pre-downloaded musl-linked Node.js binary to OUT_DIR
//!    for embedding in JS runtime images.
//!
//! On non-Linux hosts (macOS development), writes placeholder files instead.
//! The placeholders allow hyphae-core to compile on any platform, but rootfs
//! builds requiring real binaries need a Linux host or cross-compilation.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");

    build_init_binary(&out_dir);
    build_dimension_agent(&out_dir);
    prepare_node_binary(&out_dir);
}

/// Build or provide a placeholder for the hyphae-init binary.
fn build_init_binary(out_dir: &str) {
    let init_output = PathBuf::from(out_dir).join("hyphae-init");

    // Rebuild when the init binary source changes
    println!("cargo:rerun-if-changed=../hyphae-init/src/main.rs");
    println!("cargo:rerun-if-changed=../hyphae-init/Cargo.toml");

    if cfg!(target_os = "linux") {
        // On Linux, attempt to cross-compile hyphae-init for musl
        let workspace_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
            .parent()
            .and_then(|p| p.parent())
            .expect("failed to find workspace root")
            .to_path_buf();

        // Use a separate target directory to avoid deadlocks with parent cargo process
        let build_dir = workspace_root.join("target_build_script");
        let result = Command::new("cargo")
            .arg("build")
            .arg("--release")
            .arg("--target")
            .arg("x86_64-unknown-linux-musl")
            .arg("-p")
            .arg("hyphae-init")
            .arg("--target-dir")
            .arg(&build_dir)
            .current_dir(&workspace_root)
            .status();

        match result {
            Ok(status) if status.success() => {
                let binary_path = build_dir
                    .join("x86_64-unknown-linux-musl")
                    .join("release")
                    .join("hyphae-init");

                if binary_path.exists() {
                    std::fs::copy(&binary_path, &init_output)
                        .expect("failed to copy init binary to OUT_DIR");
                    return;
                }

                println!(
                    "cargo:warning=hyphae-init compiled but binary not found at {}",
                    binary_path.display()
                );
            }
            Ok(status) => {
                println!(
                    "cargo:warning=hyphae-init build failed (exit code: {}). \
                     Using placeholder init binary. Install x86_64-unknown-linux-musl \
                     target for real init binary.",
                    status
                );
            }
            Err(e) => {
                println!(
                    "cargo:warning=failed to invoke cargo for hyphae-init build: {e}. \
                     Using placeholder init binary."
                );
            }
        }
    } else {
        println!(
            "cargo:warning=non-Linux host detected. Using placeholder init binary. \
             Rootfs builds requiring a real init binary need a Linux host."
        );
    }

    // Fallback: write placeholder
    std::fs::write(&init_output, b"PLACEHOLDER_INIT_BINARY")
        .expect("failed to write placeholder init binary");
}

/// Build or provide a placeholder for the dimension-agent binary.
fn build_dimension_agent(out_dir: &str) {
    let agent_output = PathBuf::from(out_dir).join("dimension-agent");

    // Rebuild when the agent source changes
    println!("cargo:rerun-if-changed=../dimension-agent/src");
    println!("cargo:rerun-if-changed=../dimension-agent/Cargo.toml");
    println!("cargo:rerun-if-changed=../dimension-protocol/src");
    println!("cargo:rerun-if-changed=../dimension-protocol/proto");

    if cfg!(target_os = "linux") {
        let workspace_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
            .parent()
            .and_then(|p| p.parent())
            .expect("failed to find workspace root")
            .to_path_buf();

        let build_dir = workspace_root.join("target_build_script");

        // Check if the dimension-agent crate still exists before trying to compile
        let agent_crate_dir = workspace_root.join("crates/dimension-agent");
        if agent_crate_dir.exists() {
            let result = Command::new("cargo")
                .arg("build")
                .arg("--release")
                .arg("--target")
                .arg("x86_64-unknown-linux-musl")
                .arg("-p")
                .arg("dimension-agent")
                .arg("--target-dir")
                .arg(&build_dir)
                .current_dir(&workspace_root)
                .status();

            match result {
                Ok(status) if status.success() => {
                    let binary_path = build_dir
                        .join("x86_64-unknown-linux-musl")
                        .join("release")
                        .join("dimension-agent");

                    if binary_path.exists() {
                        std::fs::copy(&binary_path, &agent_output)
                            .expect("failed to copy dimension-agent binary to OUT_DIR");
                        return;
                    }

                    println!(
                        "cargo:warning=dimension-agent compiled but binary not found at {}",
                        binary_path.display()
                    );
                }
                Ok(status) => {
                    println!(
                        "cargo:warning=dimension-agent build failed (exit code: {})",
                        status
                    );
                }
                Err(e) => {
                    println!(
                        "cargo:warning=failed to invoke cargo for dimension-agent build: {e}"
                    );
                }
            }
        }

        // Fallback: use pre-built musl binary from target_build_script if available
        let prebuilt_path = build_dir
            .join("x86_64-unknown-linux-musl")
            .join("release")
            .join("dimension-agent");
        if prebuilt_path.exists() {
            println!("cargo:warning=Using pre-built dimension-agent from {}", prebuilt_path.display());
            std::fs::copy(&prebuilt_path, &agent_output)
                .expect("failed to copy pre-built dimension-agent to OUT_DIR");
            return;
        }
    } else {
        println!(
            "cargo:warning=non-Linux host detected. Using placeholder dimension-agent binary."
        );
    }

    // Fallback: write placeholder
    std::fs::write(&agent_output, b"PLACEHOLDER_DIMENSION_AGENT")
        .expect("failed to write placeholder dimension-agent binary");
}

/// Copy or provide a placeholder for the Node.js binary.
///
/// Looks for a pre-downloaded musl-linked Node.js binary at:
/// 1. `HYPHAE_NODE_BINARY` env var (if set)
/// 2. `vendor/node-linux-x64-musl` relative to workspace root
///
/// If not found, writes a placeholder so compilation succeeds on dev machines.
fn prepare_node_binary(out_dir: &str) {
    let node_output = PathBuf::from(out_dir).join("node-binary");

    println!("cargo:rerun-if-env-changed=HYPHAE_NODE_BINARY");

    let workspace_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .and_then(|p| p.parent())
        .expect("failed to find workspace root")
        .to_path_buf();

    let default_path = workspace_root.join("vendor/node-linux-x64-musl");
    println!("cargo:rerun-if-changed={}", default_path.display());

    // Check for Node.js binary: env var first, then default path
    let node_source = env::var("HYPHAE_NODE_BINARY")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .or_else(|| {
            if default_path.exists() {
                Some(default_path)
            } else {
                None
            }
        });

    match node_source {
        Some(source) => {
            std::fs::copy(&source, &node_output).unwrap_or_else(|e| {
                panic!(
                    "failed to copy Node.js binary from {} to {}: {e}",
                    source.display(),
                    node_output.display()
                )
            });
        }
        None => {
            println!(
                "cargo:warning=Node.js binary not found. Using placeholder. \
                 Set HYPHAE_NODE_BINARY env var or place binary at vendor/node-linux-x64-musl"
            );
            std::fs::write(&node_output, b"PLACEHOLDER_NODE")
                .expect("failed to write placeholder node binary");
        }
    }
}
