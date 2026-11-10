//! Asset index handling.
//!
//! Modern versions store objects in a content-addressed pool
//! (`assets/objects/ab/cdef...`) and reference them by hash. Older versions
//! ("virtual" and "map_to_resources" layouts) need a materialised tree as well.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetObject {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetIndex {
    #[serde(default)]
    pub objects: BTreeMap<String, AssetObject>,
    /// Present for pre-1.7 indexes, where the tree is materialised on disk.
    #[serde(default)]
    pub map_to_resources: Option<bool>,
    #[serde(default)]
    pub virtual_: Option<bool>,
    #[serde(rename = "virtual", default)]
    pub is_virtual: Option<bool>,
}

impl AssetIndex {
    pub fn parse(bytes: &[u8]) -> crate::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// Relative path of an object inside the content-addressed pool.
    pub fn object_path(hash: &str) -> String {
        let prefix = &hash[..2.min(hash.len())];
        format!("objects/{prefix}/{hash}")
    }

    /// `true` when a materialised resources tree must be produced.
    pub fn needs_virtual_tree(&self) -> bool {
        self.is_virtual.unwrap_or(false) || self.virtual_.unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "objects": {
            "icons/icon_16x16.png": {"hash": "abcdef0123456789", "size": 123},
            "minecraft/sounds/ambient/cave/cave1.ogg": {"hash": "ffee00112233", "size": 456}
        }
    }"#;

    #[test]
    fn parses_objects() {
        let index = AssetIndex::parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(index.objects.len(), 2);
        assert_eq!(index.objects["icons/icon_16x16.png"].size, 123);
        assert!(!index.needs_virtual_tree());
    }

    #[test]
    fn object_paths_use_two_char_prefix() {
        assert_eq!(AssetIndex::object_path("abcdef0123456789"), "objects/ab/abcdef0123456789");
    }

    #[test]
    fn virtual_flag_is_detected() {
        let index: AssetIndex = serde_json::from_str(r#"{"objects":{},"virtual":true}"#).unwrap();
        assert!(index.needs_virtual_tree());

        let legacy: AssetIndex =
            serde_json::from_str(r#"{"objects":{},"map_to_resources":true}"#).unwrap();
        assert!(!legacy.needs_virtual_tree());
        assert_eq!(legacy.map_to_resources, Some(true));
    }
}
