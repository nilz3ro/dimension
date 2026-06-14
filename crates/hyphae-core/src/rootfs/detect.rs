use std::path::Path;

use hyphae_errors::RootfsError;

/// Recognized project types for rootfs building.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectType {
    JavaScript,
    Rust,
    Binary,
    Docker,
}

/// Detect the project type by looking for marker files in `project_dir`.
///
/// - `package.json` only -> `ProjectType::JavaScript`
/// - `Cargo.toml` only -> `ProjectType::Rust`
/// - Both -> `RootfsError::AmbiguousProject`
/// - Neither -> `RootfsError::UnrecognizedProject`
pub fn detect_project_type(project_dir: &Path) -> Result<ProjectType, RootfsError> {
    let has_package_json = project_dir.join("package.json").exists();
    let has_cargo_toml = project_dir.join("Cargo.toml").exists();

    match (has_package_json, has_cargo_toml) {
        (true, true) => Err(RootfsError::AmbiguousProject {
            path: project_dir.to_path_buf(),
        }),
        (true, false) => Ok(ProjectType::JavaScript),
        (false, true) => Ok(ProjectType::Rust),
        (false, false) => Err(RootfsError::UnrecognizedProject {
            path: project_dir.to_path_buf(),
        }),
    }
}
