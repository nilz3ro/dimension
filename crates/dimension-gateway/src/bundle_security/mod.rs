//! Archive security validation for bundle uploads.
//!
//! This module provides pre-extraction validation to prevent malicious archives
//! from causing path traversal attacks, zip bombs, or other exploits.

pub mod archive;

pub use archive::{validate_and_extract, ArchiveError};
