//! Java runtime discovery, validation and provisioning.

pub mod adoptium;
pub mod discover;
pub mod selector;
pub mod version;

pub use adoptium::{JdkDownload, install as install_jdk, resolve as resolve_jdk};
pub use discover::{JavaInstallation, best_match, discover as discover_java, probe};
pub use selector::{
    JavaChoice, JavaRequirement, choose as choose_java, requirement_for_version,
    requirement_from_metadata,
};
pub use version::{major, normalize, satisfies};
