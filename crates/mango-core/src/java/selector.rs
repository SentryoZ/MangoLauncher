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
use crate::manifest::JavaSpec;

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

/// Java major version to use when a release's metadata is unavailable.
///
/// Mojang publishes the authoritative value in `version.json` as
/// `javaVersion.majorVersion`, and that is what the launcher uses. This mapping
/// only covers the legacy `1.x` scheme, where the game version identifies the
/// runtime reliably (1.16.5 and older need 8, 1.17–1.20.4 need 17, 1.20.5+
/// need 21).
///
/// Newer releases use a different scheme (`26.3`, `27.1`, …) that carries no
/// hint about the runtime, so guessing from it is impossible; 21 is returned as
/// a last resort and callers must prefer the metadata.
pub fn default_major(minecraft: &str) -> u32 {
    let (major, minor, patch) = parse_semver(minecraft);
    if major != 1 {
        // Unknown scheme: no information is encoded in the version number.
        return 21;
    }

    match minor {
        0..=16 => 8,
        17..=20 => {
            if minor == 20 && patch >= 5 {
                21
            } else {
                17
            }
        }
        _ => 21,
    }
}

/// Build a requirement from the authoritative major in `version.json`.
///
/// The range is bounded to that single major: Minecraft crashes at class
/// initialisation when run on a Java newer than the one it was compiled for
/// (`UnsupportedClassVersionError`), so "at least N" is the wrong rule.
pub fn requirement_from_metadata(major: u32) -> JavaRequirement {
    JavaRequirement { supported: bounded_range(major), suggested: major, exact_major: major }
}

/// Derive the requirement from a manifest entry, falling back to the game version.
///
/// Prefer [`requirement_from_metadata`] whenever `version.json` has been
/// fetched: it is authoritative. This function is the offline/early path used
/// before the version metadata is available, for example to show a requirement in
/// the UI.
pub fn requirement_for(minecraft: &str, spec: Option<&JavaSpec>) -> JavaRequirement {
    let default = default_major(minecraft);

    match spec {
        Some(spec) => {
            let suggested = spec.suggested.unwrap_or(default);
            JavaRequirement {
                // An explicit range from the manifest is trusted as written: the
                // server owner knows which runtimes their pack tolerates.
                supported: spec.supported.clone().unwrap_or_else(|| bounded_range(suggested)),
                suggested,
                exact_major: suggested,
            }
        }
        None => requirement_from_metadata(default),
    }
}

/// Requirement for a fetched `version.json`, preferring Mojang's own answer.
///
/// `javaVersion.majorVersion` is authoritative and works for every release
/// scheme. The manifest's range still wins when it declares one, because a server
/// owner may know their pack tolerates a different runtime.
pub fn requirement_for_version(
    minecraft: &str,
    metadata_major: Option<u32>,
    spec: Option<&JavaSpec>,
) -> JavaRequirement {
    let Some(major) = metadata_major else {
        return requirement_for(minecraft, spec);
    };

    match spec {
        Some(spec) => {
            let suggested = spec.suggested.unwrap_or(major);
            JavaRequirement {
                supported: spec.supported.clone().unwrap_or_else(|| bounded_range(major)),
                suggested,
                exact_major: major,
            }
        }
        None => requirement_from_metadata(major),
    }
}

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

fn parse_semver(version: &str) -> (u32, u32, u32) {
    let mut pieces = version.split('.');
    let major = pieces.next().and_then(|p| p.parse().ok()).unwrap_or(1);
    let minor = pieces.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = pieces.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor, patch)
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
    fn java_major_matches_mojang_mapping() {
        assert_eq!(default_major("1.7.10"), 8);
        assert_eq!(default_major("1.12.2"), 8);
        assert_eq!(default_major("1.16.5"), 8);
        assert_eq!(default_major("1.17"), 17);
        assert_eq!(default_major("1.19.4"), 17);
        assert_eq!(default_major("1.20.1"), 17);
        assert_eq!(default_major("1.20.4"), 17);
        assert_eq!(default_major("1.20.5"), 21);
        assert_eq!(default_major("1.21.1"), 21);
    }

    #[test]
    fn new_version_scheme_carries_no_java_information() {
        // Mojang moved to `26.3`-style names; the number says nothing about the
        // runtime, so callers must use `javaVersion.majorVersion`.
        assert_eq!(default_major("26.3"), 21);
        assert_eq!(default_major("26.4-snapshot-7"), 21);
    }

    #[test]
    fn metadata_major_wins_over_the_version_name() {
        // The real values Mojang publishes: 26.3 -> 25, 1.21.4 -> 21, 1.20.1 -> 17.
        assert_eq!(requirement_from_metadata(25).exact_major, 25);
        assert_eq!(requirement_from_metadata(25).supported, ">=25 <26");
        assert_eq!(requirement_from_metadata(21).suggested, 21);

        let requirement = requirement_for_version("26.3", Some(25), None);
        assert_eq!(requirement.exact_major, 25);
        assert_eq!(requirement.supported, ">=25 <26");
    }

    #[test]
    fn metadata_major_falls_back_to_the_name_without_metadata() {
        let requirement = requirement_for_version("1.20.1", None, None);
        assert_eq!(requirement.exact_major, 17);
    }

    #[test]
    fn an_explicit_manifest_range_still_wins_over_metadata() {
        let spec = JavaSpec {
            supported: Some(">=21".to_owned()),
            suggested: Some(21),
            distribution: None,
            ram: None,
        };
        let requirement = requirement_for_version("26.3", Some(25), Some(&spec));
        // The range is honoured, but the exact major stays what Mojang compiled for.
        assert_eq!(requirement.supported, ">=21");
        assert_eq!(requirement.exact_major, 25);
    }

    #[test]
    fn requirement_defaults_from_the_game_version() {
        let requirement = requirement_for("1.12.2", None);
        assert_eq!(requirement.suggested, 8);
        // Bounded to one major, so Java 17 cannot be chosen for a 1.12.2 pack.
        assert_eq!(requirement.supported, ">=8 <9");

        let requirement = requirement_for("1.20.1", None);
        assert_eq!(requirement.suggested, 17);
        assert_eq!(requirement.supported, ">=17 <18");
    }

    #[test]
    fn manifest_range_is_trusted_as_written() {
        let spec = JavaSpec {
            supported: Some(">=16 <20".to_owned()),
            suggested: Some(17),
            distribution: None,
            ram: None,
        };
        let requirement = requirement_for("1.20.1", Some(&spec));
        assert_eq!(requirement.supported, ">=16 <20");
        assert_eq!(requirement.exact_major, 17);
    }

    #[test]
    fn java_25_is_refused_for_1201() {
        // Observed in practice: 1.20.1 crashes on Java 25 inside a static
        // initialiser, so a numerically-satisfying but too-new runtime must not be
        // selected.
        let installations = vec![java("25.0.3", "java25")];
        let requirement = requirement_for("1.20.1", None);

        // ">=17" is numerically satisfied by 25, so the guard has to come from the
        // exact-major preference rather than the range.
        match choose(&installations, &requirement, None) {
            JavaChoice::NeedsDownload { major, .. } => assert_eq!(major, 17),
            other => panic!("expected a download request for java 17, got {other:?}"),
        }
    }

    #[test]
    fn exact_major_beats_a_newer_valid_range_match() {
        let installations = vec![java("17.0.9", "java17"), java("21.0.1", "java21")];
        let requirement = requirement_for("1.20.1", None);

        match choose(&installations, &requirement, None) {
            JavaChoice::Installed(java) => {
                assert_eq!(java.executable, PathBuf::from("java17"), "must not pick 21 for 1.20.1")
            }
            other => panic!("expected java17, got {other:?}"),
        }
    }

    #[test]
    fn range_fallback_allows_newer_java_when_the_exact_major_is_missing() {
        // A server that declares a wide range may accept Java 21 for a 17-era
        // game; the fallback honours that instead of forcing a download.
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
        let requirement = requirement_for("1.20.1", None);

        match choose(&installations, &requirement, None) {
            JavaChoice::Installed(java) => assert_eq!(java.executable, PathBuf::from("java17")),
            other => panic!("expected java17, got {other:?}"),
        }
    }

    #[test]
    fn requests_download_when_nothing_fits() {
        let installations = vec![java("25.0.3", "java25")];
        let requirement =
            JavaRequirement { supported: ">=17 <18".to_owned(), suggested: 17, exact_major: 17 };

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
        let requirement = requirement_for("1.20.1", None);

        match choose(&installations, &requirement, Some("java17")) {
            JavaChoice::Installed(java) => assert_eq!(java.executable, PathBuf::from("java17")),
            other => panic!("expected the pinned java, got {other:?}"),
        }
    }

    #[test]
    fn unsatisfying_pin_falls_back_to_a_matching_runtime() {
        let installations = vec![java("1.8.0_402", "java8"), java("17.0.9", "java17")];
        let requirement = requirement_for("1.20.1", None);

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
