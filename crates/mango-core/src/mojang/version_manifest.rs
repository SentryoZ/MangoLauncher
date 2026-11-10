//! Index of every published Minecraft version.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionSummary {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub release_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestVersions {
    pub release: String,
    #[serde(default)]
    pub snapshot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    pub versions: Vec<VersionSummary>,
}

impl VersionManifest {
    pub fn parse(bytes: &[u8]) -> crate::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    pub fn find(&self, id: &str) -> Option<&VersionSummary> {
        self.versions.iter().find(|version| version.id == id)
    }

    /// Newest version of a given kind (`release` or `snapshot`).
    pub fn newest_of_kind(&self, kind: &str) -> Option<&VersionSummary> {
        self.versions.iter().find(|version| version.kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "latest": {"release": "1.21.4", "snapshot": "25w02a"},
        "versions": [
            {"id": "1.21.4", "type": "release", "url": "https://example.com/1.21.4.json", "sha1": "aaa", "releaseTime": "2025-01-01T00:00:00Z"},
            {"id": "25w02a", "type": "snapshot", "url": "https://example.com/25w02a.json"}
        ]
    }"#;

    #[test]
    fn parses_and_looks_up_versions() {
        let manifest = VersionManifest::parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(manifest.latest.release, "1.21.4");
        assert_eq!(manifest.find("1.21.4").unwrap().kind, "release");
        assert_eq!(manifest.newest_of_kind("snapshot").unwrap().id, "25w02a");
        assert!(manifest.find("nope").is_none());
    }
}
