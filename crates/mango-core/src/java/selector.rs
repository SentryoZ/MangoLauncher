//! Choosing the right JVM for an instance.
//!
//! Minecraft is tightly coupled to the Java version it was built against: 1.20.1
//! needs 17, and running it on 25 crashes inside a static initialiser with
//! `Unsupported class file major version`. A launcher that picks "the newest
//! Java" therefore breaks every older instance, so the requirement is derived
//! from the game version and honoured per instance.

use tracing::{info, warn};

use crate::config::JavaConfig;
use crate::java::discover::{self, JavaInstallation};
use crate::java::version::satisfies;

/// Java requirement for an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaRequirement {
    /// npm-style semver range, e.g. `>=17`.
    pub supported: String,
    /// Major version to download when nothing local fits.
    pub suggested: u32,
    /// Major version Mojang built this release against.
    ///
    /// Newer Java is not automatically safe: 1.20.1 crashes on Java 25 inside a
    /// static initialiser (`Unsupported class file major version`), so the exact
    /// major is preferred whenever it is installed.
    pub exact_major: u32,
}

/// What the launcher decided to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaChoice {
    /// An installed runtime satisfies the requirement.
    Installed(JavaInstallation),
    /// Nothing suitable is installed and one should be downloaded.
    NeedsDownload { major: u32, reason: String },
}

/// Build a requirement from the authoritative major in `version.json`.
///
/// The range is bounded to that single major: Minecraft crashes at class
/// initialisation when run on a Java newer than the one it was compiled for
/// (`UnsupportedClassVersionError`), so "at least N" is the wrong rule.
pub fn requirement_from_metadata(major: u32) -> JavaRequirement {
    JavaRequirement { supported: bounded_range(major), suggested: major, exact_major: major }
}

/// Requirement for a Minecraft version.
///
/// `javaVersion.majorVersion` from `version.json` is the only source, because it
/// is authoritative for every release scheme: `26.3` needs 25, `1.20.1` needs 17,
/// `1.12.2` needs 8. The version id itself cannot be used — the post-`1.x` names
/// carry no runtime hint — and a manifest cannot improve on Mojang's own answer.
///
/// `metadata_major` is `None` only when the metadata has not been fetched yet.
pub fn requirement_for_version(metadata_major: Option<u32>) -> JavaRequirement {
    requirement_from_metadata(metadata_major.unwrap_or(FALLBACK_MAJOR))
}

/// Major used when the metadata truly cannot be read.
///
/// Choosing here would be a guess, so it is deliberately not a mapping from the
/// version name: 21 runs every release from 1.20.5 onward, and `ensure_java`
/// downloads the exact major before anything is launched anyway.
const FALLBACK_MAJOR: u32 = 21;

/// `major` -> `>=major <major+1`
fn bounded_range(major: u32) -> String {
    format!(">={major} <{}", major + 1)
}

/// Pick a JVM that satisfies `requirement`, or report that one must be downloaded.
///
/// Order: a user pin that fits, then the exact major Mojang targeted, then the
/// newest runtime inside the declared range. Preferring the exact major is what
/// keeps 1.20.1 off Java 25.
pub fn choose(
    installations: &[JavaInstallation],
    requirement: &JavaRequirement,
    pinned: Option<&str>,
) -> JavaChoice {
    if let Some(pinned) = pinned.filter(|path| !path.trim().is_empty()) {
        match installations.iter().find(|java| java.executable.to_string_lossy() == pinned) {
            Some(java) if java.major() == Some(requirement.exact_major) => {
                return JavaChoice::Installed(java.clone());
            }
            Some(java) => {
                warn!(
                    path = %java.executable.display(),
                    version = %java.version,
                    expected = requirement.exact_major,
                    "pinned java is not the version this instance targets"
                );
            }
            None => {
                // The pinned path may not be in the discovered list yet.
                if let Ok(java) = discover::probe(std::path::Path::new(pinned))
                    && java.major() == Some(requirement.exact_major)
                {
                    return JavaChoice::Installed(java);
                }
            }
        }
    }

    if let Some(exact) =
        installations.iter().find(|java| java.major() == Some(requirement.exact_major))
    {
        info!(version = %exact.version, major = requirement.exact_major, "selected exact java");
        return JavaChoice::Installed(exact.clone());
    }

    // Fall back to anything inside the declared range, newest first. This covers
    // servers that accept a newer Java than the one Mojang shipped.
    let mut candidates: Vec<&JavaInstallation> =
        installations.iter().filter(|java| satisfies_requirement(java, requirement)).collect();
    candidates.sort_by_key(|java| std::cmp::Reverse(java.major()));

    if let Some(chosen) = candidates.first() {
        info!(
            version = %chosen.version,
            required = %requirement.supported,
            "selected java from range"
        );
        return JavaChoice::Installed((*chosen).clone());
    }

    let found: Vec<String> = installations.iter().map(|java| java.version.clone()).collect();
    JavaChoice::NeedsDownload {
        major: requirement.suggested,
        reason: format!(
            "no installed Java matches Java {} (found: {})",
            requirement.suggested,
            if found.is_empty() { "none".to_owned() } else { found.join(", ") }
        ),
    }
}

fn satisfies_requirement(java: &JavaInstallation, requirement: &JavaRequirement) -> bool {
    java.normalized().map(|version| satisfies(&version, &requirement.supported)).unwrap_or(false)
}

/// Memory arguments for an instance.
pub fn memory_arguments(config: &JavaConfig) -> Vec<String> {
    vec![format!("-Xmx{}", config.max_ram), format!("-Xms{}", config.min_ram)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn java(version: &str, name: &str) -> JavaInstallation {
        JavaInstallation {
            executable: PathBuf::from(name),
            version: version.to_owned(),
            vendor: "test".to_owned(),
            arch: "amd64".to_owned(),
        }
    }

    #[test]
    fn the_requirement_comes_straight_from_mojang_metadata() {
        // The values Mojang actually publishes in `javaVersion.majorVersion`.
        assert_eq!(requirement_for_version(Some(8)).exact_major, 8);
        assert_eq!(requirement_for_version(Some(17)).exact_major, 17);
        assert_eq!(requirement_for_version(Some(21)).exact_major, 21);
        assert_eq!(requirement_for_version(Some(25)).exact_major, 25);
    }

    #[test]
    fn the_range_is_bounded_to_one_major() {
        // "at least 17" would let Java 25 through, which crashes 1.20.1.
        assert_eq!(requirement_for_version(Some(17)).supported, ">=17 <18");
        assert_eq!(requirement_for_version(Some(25)).supported, ">=25 <26");
        assert_eq!(requirement_for_version(Some(8)).supported, ">=8 <9");
    }

    #[test]
    fn a_26_3_release_gets_java_25_not_the_fallback() {
        // The regression this module exists to prevent: the launch used to guess
        // 21 from the version id and the game died with
        // `Unsupported class file major version 69`.
        let requirement = requirement_for_version(Some(25));
        assert_eq!(requirement.exact_major, 25);
        assert_eq!(requirement.supported, ">=25 <26");
    }

    #[test]
    fn missing_metadata_falls_back_without_guessing_from_the_version_name() {
        // The old `default_major` mapped `1.12.2` to 8 from the id. That mapping is
        // gone: the fallback is a constant, because guessing from the name is what
        // produced the wrong runtime in the first place.
        let requirement = requirement_for_version(None);
        assert_eq!(requirement.exact_major, FALLBACK_MAJOR);
        assert_eq!(requirement.suggested, FALLBACK_MAJOR);
    }

    #[test]
    fn java_25_is_refused_for_1201() {
        // Observed in practice: 1.20.1 crashes on Java 25 inside a static
        // initialiser, so a numerically-satisfying but too-new runtime must not be
        // selected.
        let installations = vec![java("25.0.3", "java25")];
        let requirement = requirement_for_version(Some(17));

        match choose(&installations, &requirement, None) {
            JavaChoice::NeedsDownload { major, .. } => assert_eq!(major, 17),
            other => panic!("expected a download request for java 17, got {other:?}"),
        }
    }

    #[test]
    fn exact_major_beats_a_newer_valid_range_match() {
        let installations = vec![java("17.0.9", "java17"), java("21.0.1", "java21")];
        let requirement = requirement_for_version(Some(17));

        match choose(&installations, &requirement, None) {
            JavaChoice::Installed(java) => {
                assert_eq!(java.executable, PathBuf::from("java17"), "must not pick 21 for 1.20.1")
            }
            other => panic!("expected java17, got {other:?}"),
        }
    }

    #[test]
    fn range_fallback_allows_newer_java_when_the_exact_major_is_missing() {
        let installations = vec![java("21.0.1", "java21")];
        let requirement =
            JavaRequirement { supported: ">=17".to_owned(), suggested: 17, exact_major: 17 };

        match choose(&installations, &requirement, None) {
            JavaChoice::Installed(java) => assert_eq!(java.executable, PathBuf::from("java21")),
            other => panic!("expected a range fallback, got {other:?}"),
        }
    }

    #[test]
    fn chooses_the_exact_major_when_available() {
        let installations =
            vec![java("1.8.0_402", "java8"), java("17.0.9", "java17"), java("21.0.1", "java21")];
        let requirement = requirement_for_version(Some(17));

        match choose(&installations, &requirement, None) {
            JavaChoice::Installed(java) => assert_eq!(java.executable, PathBuf::from("java17")),
            other => panic!("expected java17, got {other:?}"),
        }
    }

    #[test]
    fn requests_download_when_nothing_fits() {
        let installations = vec![java("25.0.3", "java25")];
        let requirement = requirement_for_version(Some(17));

        match choose(&installations, &requirement, None) {
            JavaChoice::NeedsDownload { major, reason } => {
                assert_eq!(major, 17);
                assert!(reason.contains("no installed Java"), "{reason}");
            }
            other => panic!("expected a download request, got {other:?}"),
        }
    }

    #[test]
    fn pinned_java_is_used_when_it_fits() {
        let installations = vec![java("17.0.9", "java17"), java("21.0.1", "java21")];
        let requirement = requirement_for_version(Some(17));

        match choose(&installations, &requirement, Some("java17")) {
            JavaChoice::Installed(java) => assert_eq!(java.executable, PathBuf::from("java17")),
            other => panic!("expected the pinned java, got {other:?}"),
        }
    }

    #[test]
    fn unsatisfying_pin_falls_back_to_a_matching_runtime() {
        let installations = vec![java("1.8.0_402", "java8"), java("17.0.9", "java17")];
        let requirement = requirement_for_version(Some(17));

        match choose(&installations, &requirement, Some("java8")) {
            JavaChoice::Installed(java) => assert_eq!(java.executable, PathBuf::from("java17")),
            other => panic!("expected a fallback, got {other:?}"),
        }
    }

    #[test]
    fn memory_arguments_are_rendered() {
        let config =
            JavaConfig { min_ram: "2G".to_owned(), max_ram: "4G".to_owned(), ..Default::default() };
        assert_eq!(memory_arguments(&config), vec!["-Xmx4G", "-Xms2G"]);
    }
}
