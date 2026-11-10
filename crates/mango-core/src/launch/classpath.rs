//! Classpath assembly.
//!
//! Three rules make this non-trivial:
//!
//! * The vanilla client jar is on the classpath, except for Forge 1.17+, where
//!   SecureJarHandler takes over and a stray vanilla jar breaks loading.
//! * Server-declared libraries override Mojang libraries with the same
//!   `group:artifact`, keeping the server's pinned version.
//! * Entries must end in `.jar`; classifier-suffixed maven names sometimes leak
//!   through in profiles.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One entry to place on the classpath.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClasspathEntry {
    pub path: PathBuf,
    /// `group:artifact`, used for overriding.
    pub versionless_id: Option<String>,
}

impl ClasspathEntry {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), versionless_id: None }
    }

    pub fn with_id(path: impl Into<PathBuf>, id: impl Into<String>) -> Self {
        Self { path: path.into(), versionless_id: Some(id.into()) }
    }
}

/// Merge Mojang and server entries into a deterministic classpath.
///
/// Insertion order is not meaningful here: the list is keyed by versionless
/// maven id so server libraries can override Mojang ones, and sorted output
/// keeps launch commands reproducible for log comparison.
pub fn build(
    client_jar: Option<&Path>,
    mojang: &[ClasspathEntry],
    server: &[ClasspathEntry],
) -> Vec<PathBuf> {
    let mut resolved: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut ordered: Vec<PathBuf> = Vec::new();

    // Mojang libraries first so server libraries can override them.
    for entry in mojang {
        let key = entry
            .versionless_id
            .clone()
            .unwrap_or_else(|| entry.path.to_string_lossy().to_string());
        resolved.insert(key, entry.path.clone());
    }
    for entry in server {
        let key = entry
            .versionless_id
            .clone()
            .unwrap_or_else(|| entry.path.to_string_lossy().to_string());
        resolved.insert(key, entry.path.clone());
    }

    if let Some(client_jar) = client_jar {
        ordered.push(client_jar.to_path_buf());
    }
    ordered.extend(resolved.into_values());

    dedupe_and_trim(ordered)
}

/// Render paths as a classpath string.
pub fn to_classpath_string(entries: &[PathBuf], separator: &str) -> String {
    entries.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(separator)
}

/// Drop duplicates and clamp every entry to end at `.jar`.
fn dedupe_and_trim(entries: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<PathBuf> = Vec::new();

    for entry in entries {
        let trimmed = trim_to_jar(&entry);
        let key = trimmed.to_string_lossy().to_ascii_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(trimmed);
    }

    out
}

/// Forge profiles occasionally append a classifier after `.jar`; cut it off.
fn trim_to_jar(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.to_ascii_lowercase().find(".jar") {
        Some(index) if index + 4 != text.len() => PathBuf::from(&text[..index + 4]),
        _ => path.to_path_buf(),
    }
}

/// Whether the vanilla client jar belongs on the classpath.
///
/// Forge 1.17 and newer must not see it; Fabric still needs it.
pub fn include_client_jar(mc_version: &str, loader: LoaderClasspath) -> bool {
    match loader {
        LoaderClasspath::Vanilla | LoaderClasspath::Fabric | LoaderClasspath::Quilt => true,
        LoaderClasspath::Forge | LoaderClasspath::NeoForge => {
            major_minor(mc_version).is_none_or(|(major, minor)| (major, minor) < (1, 17))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderClasspath {
    Vanilla,
    Fabric,
    Quilt,
    Forge,
    NeoForge,
}

fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut pieces = version.split('.');
    let major = pieces.next()?.parse().ok()?;
    let minor = pieces.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jar(name: &str) -> PathBuf {
        PathBuf::from(format!("/libs/{name}"))
    }

    #[test]
    fn client_jar_leads_the_classpath() {
        let entries = build(
            Some(&jar("1.20.1.jar")),
            &[ClasspathEntry::with_id(jar("guava.jar"), "com.google.guava:guava")],
            &[],
        );
        assert_eq!(entries[0], jar("1.20.1.jar"));
        assert!(entries.contains(&jar("guava.jar")));
    }

    #[test]
    fn server_libraries_override_mojang_ones() {
        let entries = build(
            None,
            &[ClasspathEntry::with_id(jar("guava-31.jar"), "com.google.guava:guava")],
            &[ClasspathEntry::with_id(jar("guava-17.jar"), "com.google.guava:guava")],
        );
        assert_eq!(entries, vec![jar("guava-17.jar")]);
    }

    #[test]
    fn duplicates_are_removed_case_insensitively() {
        let entries =
            build(Some(&jar("Client.JAR")), &[ClasspathEntry::new(jar("client.jar"))], &[]);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn entries_are_trimmed_at_the_jar_extension() {
        let entries = build(None, &[ClasspathEntry::new("/libs/lwjgl.jar.classifier")], &[]);
        assert_eq!(entries[0], PathBuf::from("/libs/lwjgl.jar"));
    }

    #[test]
    fn classpath_string_uses_platform_separator() {
        let entries = vec![jar("a.jar"), jar("b.jar")];
        assert_eq!(to_classpath_string(&entries, ";"), "/libs/a.jar;/libs/b.jar");
        assert_eq!(to_classpath_string(&entries, ":"), "/libs/a.jar:/libs/b.jar");
    }

    #[test]
    fn client_jar_is_kept_for_fabric_and_dropped_for_modern_forge() {
        assert!(include_client_jar("1.20.1", LoaderClasspath::Fabric));
        assert!(!include_client_jar("1.20.1", LoaderClasspath::Forge));
        assert!(!include_client_jar("1.20.1", LoaderClasspath::NeoForge));
        assert!(include_client_jar("1.12.2", LoaderClasspath::Forge));
        assert!(include_client_jar("1.16.5", LoaderClasspath::Forge));
        assert!(include_client_jar("1.20.1", LoaderClasspath::Vanilla));
    }
}
