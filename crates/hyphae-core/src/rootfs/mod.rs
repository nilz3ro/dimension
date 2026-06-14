pub mod binary;
pub mod detect;
pub mod docker;
pub mod image;
pub mod init;
pub mod js;
pub mod rust_build;
pub mod staging;

pub use binary::prepare_binary_runtime;
pub use detect::{ProjectType, detect_project_type};
pub use image::{BuildConfig, BuildResult, build_rootfs, check_disk_space};
pub use init::{embed_dimension_agent, embed_init, embed_init_from_bytes, is_dimension_agent_real, is_init_real, validate_dimension_agent, validate_init, write_env_config};
pub use js::{detect_js_entrypoint, prepare_js_runtime};
pub use rust_build::{build_rust_project, detect_rust_binary, prepare_rust_runtime};
pub use staging::create_directory_structure;
