//! Java version parsing.
//!
//! Manifests declare a semver range but the JVM reports its own formats:
//! `1.8.0_152-b16` for Java 8 and below, `17.0.9+9` for Java 9+. Both are
//! normalised to semver so range checks behave consistently.

use semver::{Version, VersionReq};

use crate::error::{MangoError, Result};

/// Normalise a JVM version string into semver.
///
/// Java 8 and older are reported as `1.{major}.{minor}_{patch}-b{build}`; the
/// leading `1` is dropped so `1.8.0_152` matches a `>=8` requirement.
pub fn normalize(raw: &str) -> Result<Version> {
    let raw = raw.trim();

    // `1.8.0_152-b16` / `1.8.0_152` / `1.8`
    if let Some(rest) = raw.strip_prefix("1.")
        && rest.chars().next().is_some_and(|c| c.is_ascii_digit() && c < '9')
    {
        let (version_part, build) = match raw.split_once("-b") {
            Some((left, build)) => (left, Some(build)),
            None => (raw, None),
        };

        let mut segments = version_part.split('_');
        let dotted = segments.next().unwrap_or_default();
        let patch = segments.next();

        let mut pieces: Vec<&str> = dotted.split('.').collect();
        if pieces.first() == Some(&"1") {
            pieces.remove(0);
        }
        let major = pieces.first().copied().unwrap_or("0");
        let minor = pieces.get(1).copied().unwrap_or("0");
        let patch_number = patch.or_else(|| pieces.get(2).copied()).unwrap_or("0");

        let mut text = format!("{major}.{minor}.{patch_number}");
        if let Some(build) = build
            && !build.is_empty()
        {
            text.push('+');
            text.push_str(build);
        }
        return Version::parse(&text)
            .map_err(|err| MangoError::Other(format!("bad java version `{raw}`: {err}")));
    }

    // `17.0.9+9`, `21`, `21.0.1`
    let (numeric, build) = match raw.split_once(['+', '-']) {
        Some((left, right)) => (left, Some(right)),
        None => (raw, None),
    };

    let mut pieces: Vec<String> = numeric.split('.').map(str::to_owned).collect();
    match pieces.len() {
        1 => {
            pieces.push("0".to_owned());
            pieces.push("0".to_owned());
        }
        2 => pieces.push("0".to_owned()),
        _ => {}
    }

    let mut text = pieces.join(".");
    if let Some(build) = build
        && !build.is_empty()
    {
        text.push('+');
        text.push_str(build);
    }
    Version::parse(&text)
        .map_err(|err| MangoError::Other(format!("bad java version `{raw}`: {err}")))
}

/// Major version of a normalised Java version.
pub fn major(version: &Version) -> u32 {
    u32::try_from(version.major).unwrap_or(u32::MAX)
}

/// Whether `version` satisfies a manifest's `supported` range.
///
/// Manifests use npm-style ranges (`>=16 <20`, `^17`, `~1.8`), while the
/// `semver` crate expects comma-separated comparators, so the range is
/// translated first.
pub fn satisfies(version: &Version, requirement: &str) -> bool {
    // `a || b` is a disjunction: any matching alternative is enough.
    requirement.split("||").any(|alternative| match translate_requirement(alternative) {
        Some(translated) => match VersionReq::parse(&translated) {
            Ok(req) => req.matches(version),
            Err(_) => false,
        },
        None => false,
    })
}

/// Convert an npm-style range into `semver` crate syntax.
///
/// * `>=16 <20`      becomes `>=16.0.0, <20.0.0`
/// * `^17.0.1`       becomes `>=17.0.1, <18.0.0`
/// * `~1.8`          becomes `>=1.8.0, <1.9.0`
/// * `1.2.3`         becomes `=1.2.3`
///
/// `||` alternatives are handled by [`satisfies`] before this is called.
pub fn translate_requirement(requirement: &str) -> Option<String> {
    let requirement = requirement.trim();
    if requirement.is_empty() {
        return None;
    }

    let mut comparators: Vec<String> = Vec::new();

    for token in requirement.split_whitespace() {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }

        let (operator, rest) = split_operator(token);
        let (major, minor, patch, has_minor, has_patch) = parse_partial(rest)?;

        match operator {
            "^" => {
                comparators.push(format!(">={major}.{minor}.{patch}"));
                let upper = if major > 0 {
                    format!("<{}.0.0", major + 1)
                } else if minor > 0 {
                    format!("<0.{}.0", minor + 1)
                } else {
                    format!("<0.0.{}", patch + 1)
                };
                comparators.push(upper);
            }
            "~" => {
                comparators.push(format!(">={major}.{minor}.{patch}"));
                let upper = if has_minor {
                    format!("<{}.{}.0", major, minor + 1)
                } else {
                    format!("<{}.0.0", major + 1)
                };
                comparators.push(upper);
            }
            ">=" | "<=" | ">" | "<" | "=" => {
                comparators.push(format!("{operator}{major}.{minor}.{patch}"));
            }
            "" => {
                // A bare version means exactly that version, but a partial one
                // means "compatible with", which the caller rarely intends;
                // treating it as equality is the safest reading.
                let _ = (has_minor, has_patch);
                comparators.push(format!("={major}.{minor}.{patch}"));
            }
            _ => return None,
        }
    }

    if comparators.is_empty() { None } else { Some(comparators.join(", ")) }
}

fn split_operator(token: &str) -> (&str, &str) {
    for operator in [">=", "<=", "^", "~", ">", "<", "="] {
        if let Some(rest) = token.strip_prefix(operator) {
            return (operator, rest);
        }
    }
    ("", token)
}

fn parse_partial(text: &str) -> Option<(u64, u64, u64, bool, bool)> {
    let mut pieces = text.split('.');
    let major: u64 = pieces.next()?.parse().ok()?;
    let minor_piece = pieces.next();
    let patch_piece = pieces.next();
    if pieces.next().is_some() {
        return None;
    }

    let minor = match minor_piece {
        Some(value) => value.parse().ok()?,
        None => 0,
    };
    let patch = match patch_piece {
        Some(value) => {
            // Trailing build metadata such as `9+9` is irrelevant for ranges.
            let cleaned = value.split(['+', '-']).next().unwrap_or(value);
            cleaned.parse().ok()?
        }
        None => 0,
    };

    Some((major, minor, patch, minor_piece.is_some(), patch_piece.is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_8_is_normalised_without_leading_one() {
        let version = normalize("1.8.0_152-b16").unwrap();
        assert_eq!(version.major, 8);
        assert_eq!(version.minor, 0);
        assert_eq!(version.patch, 152);
        assert_eq!(major(&version), 8);
    }

    #[test]
    fn short_java_8_forms_work() {
        assert_eq!(normalize("1.8").unwrap().major, 8);
        assert_eq!(normalize("1.8.0").unwrap().major, 8);
        assert_eq!(normalize("1.8.0_402").unwrap().patch, 402);
    }
    #[test]
    fn modern_versions_are_semver_already() {
        let version = normalize("17.0.9+9").unwrap();
        assert_eq!((version.major, version.minor, version.patch), (17, 0, 9));

        assert_eq!(normalize("21").unwrap().major, 21);
        assert_eq!(normalize("21.0.1").unwrap().patch, 1);
    }

    #[test]
    fn requirements_match_normalised_versions() {
        let java8 = normalize("1.8.0_402").unwrap();
        let java17 = normalize("17.0.9+9").unwrap();

        assert!(satisfies(&java8, ">=1.8"));
        assert!(satisfies(&java8, ">=8"));
        assert!(!satisfies(&java8, ">=17"));
        assert!(satisfies(&java17, ">=17"));
        assert!(satisfies(&java17, ">=16 <20"));
        assert!(!satisfies(&java17, ">=21"));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(normalize("").is_err());
        assert!(normalize("not-a-version").is_err());
    }

    #[test]
    fn npm_style_ranges_are_translated() {
        assert_eq!(translate_requirement(">=16 <20").as_deref(), Some(">=16.0.0, <20.0.0"));
        assert_eq!(translate_requirement("^17.0.1").as_deref(), Some(">=17.0.1, <18.0.0"));
        assert_eq!(translate_requirement("~1.8").as_deref(), Some(">=1.8.0, <1.9.0"));
        assert_eq!(translate_requirement(">=17").as_deref(), Some(">=17.0.0"));
        assert_eq!(translate_requirement("17.0.9").as_deref(), Some("=17.0.9"));
        assert!(translate_requirement("").is_none());
    }

    #[test]
    fn caret_ranges_respect_zero_majors() {
        let java17 = normalize("17.0.9+9").unwrap();
        assert!(satisfies(&java17, "^17.0.1"));
        assert!(!satisfies(&java17, "^18.0.0"));

        let java021 = normalize("0.21.0").unwrap();
        let zero_major = translate_requirement("^0.21.0").unwrap();
        assert!(VersionReq::parse(&zero_major).unwrap().matches(&java021));
    }

    #[test]
    fn or_ranges_match_any_alternative() {
        let java17 = normalize("17.0.9+9").unwrap();
        assert!(satisfies(&java17, ">=21 || >=17"));
        assert!(satisfies(&java17, ">=17 || >=21"));
        assert!(!satisfies(&java17, ">=21 || >=25"));
    }
}
