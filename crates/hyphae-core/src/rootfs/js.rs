//! JavaScript project entrypoint detection and runtime preparation.
//!
//! Detects the application entrypoint from `package.json` using a priority chain:
//! 1. `scripts.start` -- split into command parts
//! 2. `main` field -- run with `node`
//! 3. `index.js` file in project root -- run with `node`
//!
//! The Node.js binary is embedded at build time via `include_bytes!` from the
//! build.rs script. On Linux hosts, this is a real musl-linked binary; on other
//! platforms it is a placeholder.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde::Deserialize;
use walkdir::WalkDir;

use hyphae_errors::RootfsError;

/// The pre-compiled Node.js binary, embedded at build time.
///
/// On Linux hosts with the musl binary available, this is the real
/// Node.js binary. On other platforms, this is a placeholder.
const NODE_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/node-binary"));

#[derive(Deserialize)]
struct PackageJson {
    #[serde(default)]
    scripts: Option<Scripts>,
    #[serde(default)]
    main: Option<String>,
}

#[derive(Deserialize)]
struct Scripts {
    #[serde(default)]
    start: Option<String>,
}

/// Detect the JS entrypoint from a project directory containing `package.json`.
///
/// Priority:
/// 1. `scripts.start` exists -> split on whitespace and return parts
/// 2. `main` field exists -> `["node", main]`
/// 3. `index.js` exists in project_dir -> `["node", "index.js"]`
/// 4. None of the above -> `Err(NoEntrypoint)`
///
/// Also returns `PackageJsonRead` if the file cannot be read, or
/// `PackageJsonParse` if the JSON is invalid.
pub fn detect_js_entrypoint(project_dir: &Path) -> Result<Vec<String>, RootfsError> {
    let pkg_path = project_dir.join("package.json");

    let content = fs::read_to_string(&pkg_path).map_err(RootfsError::PackageJsonRead)?;

    let pkg: PackageJson = serde_json::from_str(&content)
        .map_err(|e| RootfsError::PackageJsonParse(e.to_string()))?;

    // Priority 1: scripts.start
    if let Some(scripts) = &pkg.scripts {
        if let Some(start) = &scripts.start {
            let parts: Vec<String> = start.split_whitespace().map(String::from).collect();
            if !parts.is_empty() {
                return Ok(parts);
            }
        }
    }

    // Priority 2: main field
    if let Some(main) = &pkg.main {
        if !main.is_empty() {
            return Ok(vec!["node".to_string(), main.clone()]);
        }
    }

    // Priority 3: index.js exists
    if project_dir.join("index.js").exists() {
        return Ok(vec!["node".to_string(), "index.js".to_string()]);
    }

    Err(RootfsError::NoEntrypoint {
        project_dir: project_dir.to_path_buf(),
    })
}

/// Directories to exclude when copying project files into the staging area.
const EXCLUDED_DIRS: &[&str] = &[".git", "node_modules/.cache", "target"];

/// Prepare the JS runtime in a staging directory.
///
/// 1. Writes the embedded Node.js binary to `staging_dir/usr/local/bin/node` (mode 0o755)
/// 2. Copies project files to `staging_dir/app/`, excluding `.git`, `node_modules/.cache`, `target`
/// 3. Detects the entrypoint via [`detect_js_entrypoint`]
/// 4. Returns the entrypoint command parts with absolute paths for the VM image
///
/// The returned command is suitable for passing to [`embed_init`](super::init::embed_init).
pub fn prepare_js_runtime(
    staging_dir: &Path,
    project_dir: &Path,
) -> Result<Vec<String>, RootfsError> {
    // 1. Write Node.js binary
    let node_dir = staging_dir.join("usr/local/bin");
    fs::create_dir_all(&node_dir)?;
    let node_path = node_dir.join("node");
    fs::write(&node_path, NODE_BINARY)?;
    fs::set_permissions(&node_path, fs::Permissions::from_mode(0o755))?;

    // 2. Copy project files to staging/app/
    let app_dir = staging_dir.join("app");
    fs::create_dir_all(&app_dir)?;
    copy_project_files(project_dir, &app_dir)?;

    // 3. Detect entrypoint (run against the copied files in staging/app)
    let entrypoint = detect_js_entrypoint(&app_dir)?;

    // 4. Convert to absolute paths inside the image
    //    The entrypoint from detect_js_entrypoint may be:
    //    - ["node", "server.js"] -> ["/usr/local/bin/node", "/app/server.js"]
    //    - ["node", "app.js"]   -> ["/usr/local/bin/node", "/app/app.js"]
    let cmd = convert_js_entrypoint_to_image_paths(&entrypoint);

    Ok(cmd)
}

/// Convert a detected JS entrypoint to absolute paths inside the VM image.
///
/// Replaces "node" with "/usr/local/bin/node" and prepends "/app/" to relative file paths.
fn convert_js_entrypoint_to_image_paths(entrypoint: &[String]) -> Vec<String> {
    entrypoint
        .iter()
        .enumerate()
        .map(|(i, part)| {
            if part == "node" {
                "/usr/local/bin/node".to_string()
            } else if i > 0 && !part.starts_with('/') {
                format!("/app/{part}")
            } else {
                part.clone()
            }
        })
        .collect()
}

/// Copy project files from `src` to `dst`, excluding directories in [`EXCLUDED_DIRS`].
fn copy_project_files(src: &Path, dst: &Path) -> Result<(), RootfsError> {
    for entry in WalkDir::new(src).min_depth(1).into_iter().filter_entry(|e| {
        // Skip excluded directories
        if e.file_type().is_dir() {
            let relative = e
                .path()
                .strip_prefix(src)
                .unwrap_or(e.path());
            let rel_str = relative.to_string_lossy();
            !EXCLUDED_DIRS.iter().any(|excl| {
                rel_str == *excl || rel_str.starts_with(&format!("{excl}/"))
            })
        } else {
            true
        }
    }) {
        let entry = entry.map_err(|e| RootfsError::StagingWalk(e.to_string()))?;
        let relative = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| RootfsError::StagingWalk(e.to_string()))?;
        let dest_path = dst.join(relative);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &dest_path)?;
        }
    }

    Ok(())
}
