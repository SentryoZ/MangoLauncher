//! Networking primitives: HTTP client, hash verification, download execution.

pub mod downloader;
pub mod hash;
pub mod http;

pub use downloader::{FileHash, FileKind, PlannedFile, Progress, ScanReport};
pub use hash::{HashAlgorithm, hash_bytes, hash_file, verify_file};
