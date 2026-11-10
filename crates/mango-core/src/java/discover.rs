//! Discovering installed Java runtimes.

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::debug;

/// A usable JVM found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaInstallation {
    /// Path to `java` / `java.exe`.
    pub executable: PathBuf,
    /// Raw `java.version` value reported by the JVM.
    pub version: String,
    pub vendor: String,
    pub arch: String,
}

impl JavaInstallation {
    pub fn normalized(&self) -> crate::Result<semver::Version> {
        super::version::normalize(&self.version)
    }

    pub fn major(&self) -> Option<u32> {
        self.normalized().ok().map(|version| super::version::major(&version))
    }
}

/// Filename of the JVM launcher on this platform.
pub fn java_binary_name() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

/// Read `java.version`, `java.vendor`, and `os.arch` from a JVM.
///
/// `-XshowSettings:properties` prints to stderr, which is why the output is
/// captured from both streams.
pub fn probe(executable: &Path) -> crate::Result<JavaInstallation> {
    let mut command = Command::new(executable);
    command.arg("-XshowSettings:properties").arg("-version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let output = command.output().map_err(|err| {
        crate::MangoError::Other(format!("could not run {}: {err}", executable.display()))
    })?;

    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let version = property(&text, "java.version").ok_or_else(|| {
        crate::MangoError::Other(format!("{} has no java.version", executable.display()))
    })?;
    let vendor = property(&text, "java.vendor").unwrap_or_else(|| "unknown".to_owned());
    let arch = property(&text, "os.arch").unwrap_or_else(|| "unknown".to_owned());

    Ok(JavaInstallation { executable: executable.to_path_buf(), version, vendor, arch })
}

/// Pull a `key = value` property out of `-XshowSettings` output.
fn property(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix(key) else { continue };
        let Some(value) = rest.trim_start().strip_prefix('=') else { continue };
        return Some(value.trim().to_owned());
    }
    None
}

/// Candidate `java` executables to probe, in priority order.
pub fn candidates() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();

    // JAVA_HOME wins: users set it deliberately.
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        push_bin(&mut found, PathBuf::from(home));
    }

    // Anything already on PATH.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(java_binary_name());
            if candidate.is_file() {
                push_unique(&mut found, candidate);
            }
        }
    }

    for root in known_roots() {
        collect_under(&mut found, &root, 3);
    }

    debug!(count = found.len(), "java candidates discovered");
    found
}

fn push_bin(found: &mut Vec<PathBuf>, home: PathBuf) {
    for relative in ["bin", "Contents/Home/bin"] {
        let candidate = home.join(relative).join(java_binary_name());
        if candidate.is_file() {
            push_unique(found, candidate);
        }
    }
}

fn push_unique(found: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !found.iter().any(|existing| existing == &candidate) {
        found.push(candidate);
    }
}

fn known_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if cfg!(windows) {
        for base in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Some(value) = std::env::var_os(base) {
                let base = PathBuf::from(value);
                roots.push(base.join("Java"));
                roots.push(base.join("Eclipse Adoptium"));
                roots.push(base.join("Microsoft"));
                roots.push(base.join("Amazon Corretto"));
                roots.push(base.join("Zulu"));
            }
        }
    } else if cfg!(target_os = "macos") {
        roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
    } else {
        roots.push(PathBuf::from("/usr/lib/jvm"));
    }

    // IDE- and toolchain-managed JDKs, which are the common case on developer
    // machines and live outside the vendor directories above.
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let home = PathBuf::from(home);
        roots.push(home.join(".jdks"));
        roots.push(home.join(".sdkman").join("candidates").join("java"));
    }

    roots
}

/// Recursively look for `bin/java` under a vendor directory, bounded in depth.
fn collect_under(found: &mut Vec<PathBuf>, dir: &Path, depth: u32) {
    if depth == 0 {
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else { continue };

        if file_type.is_dir() {
            push_bin(found, path.clone());
            collect_under(found, &path, depth - 1);
        } else if file_type.is_file() && path.file_name().is_some_and(|n| n == java_binary_name()) {
            // Only accept `.../bin/java`.
            if path.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "bin") {
                push_unique(found, path);
            }
        }
    }
}

/// Probe every candidate, dropping the ones that fail.
pub fn discover() -> Vec<JavaInstallation> {
    let mut installations: Vec<JavaInstallation> = Vec::new();
    for candidate in candidates() {
        match probe(&candidate) {
            Ok(installation) => {
                if !installations.iter().any(|i| i.executable == installation.executable) {
                    installations.push(installation);
                }
            }
            Err(err) => debug!(path = %candidate.display(), error = %err, "java probe failed"),
        }
    }
    installations
}

/// Pick the newest JVM satisfying `requirement`, when given.
///
/// Falls back to the newest JVM of any version so the caller can report a
/// helpful mismatch instead of "no java found".
pub fn best_match(
    installations: &[JavaInstallation],
    requirement: Option<(&str, u32)>,
) -> Option<JavaInstallation> {
    let mut sorted: Vec<&JavaInstallation> = installations.iter().collect();
    sorted.sort_by_key(|install| std::cmp::Reverse(install.major()));

    if let Some((range, suggested)) = requirement {
        if let Some(found) = sorted.iter().find(|install| {
            install
                .normalized()
                .map(|version| super::version::satisfies(&version, range))
                .unwrap_or(false)
        }) {
            return Some((*found).clone());
        }
        // Prefer the requested major even if the range is loose.
        if let Some(found) = sorted.iter().find(|install| install.major() == Some(suggested)) {
            return Some((*found).clone());
        }
    }

    sorted.first().map(|install| (*install).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_are_parsed_from_settings_output() {
        let text =
            "    java.version = 17.0.9\n    java.vendor = Eclipse Adoptium\n    os.arch = amd64\n";
        assert_eq!(property(text, "java.version").as_deref(), Some("17.0.9"));
        assert_eq!(property(text, "java.vendor").as_deref(), Some("Eclipse Adoptium"));
        assert_eq!(property(text, "os.arch").as_deref(), Some("amd64"));
        assert_eq!(property(text, "java.home"), None);
    }

    #[test]
    fn best_match_prefers_satisfying_version() {
        let make = |version: &str, name: &str| JavaInstallation {
            executable: PathBuf::from(name),
            version: version.to_owned(),
            vendor: "test".to_owned(),
            arch: "amd64".to_owned(),
        };

        let installs =
            vec![make("1.8.0_402", "java8"), make("17.0.9", "java17"), make("21.0.1", "java21")];

        let chosen = best_match(&installs, Some((">=17", 17))).unwrap();
        assert_eq!(chosen.executable, PathBuf::from("java21"));

        let java8 = best_match(&installs, Some((">=1.8 <9", 8))).unwrap();
        assert_eq!(java8.executable, PathBuf::from("java8"));

        let fallback = best_match(&installs, Some((">=30", 30))).unwrap();
        assert_eq!(fallback.executable, PathBuf::from("java21"));
    }

    #[test]
    fn best_match_without_requirement_returns_newest() {
        let make = |version: &str, name: &str| JavaInstallation {
            executable: PathBuf::from(name),
            version: version.to_owned(),
            vendor: "test".to_owned(),
            arch: "amd64".to_owned(),
        };
        let installs = vec![make("8.0.402", "old"), make("17.0.9", "new")];
        assert_eq!(best_match(&installs, None).unwrap().executable, PathBuf::from("new"));
    }

    #[test]
    fn empty_installation_list_has_no_match() {
        assert!(best_match(&[], Some((">=17", 17))).is_none());
    }
}
