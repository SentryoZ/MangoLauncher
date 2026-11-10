//! Mojang launcher metadata: version manifests, `version.json`, rules, assets.

pub mod asset_index;
pub mod maven;
pub mod rules;
pub mod version_json;
pub mod version_manifest;

pub use asset_index::{AssetIndex, AssetObject};
pub use maven::MavenCoordinate;
pub use rules::{Arch, Features, OsName, Rule, RuleContext, rules_allow};
pub use version_json::{Argument, Library, VersionJson};
pub use version_manifest::{VersionManifest, VersionSummary};
