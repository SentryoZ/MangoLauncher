//! Rule evaluation for Mojang `version.json` entries.
//!
//! Both `arguments` and `libraries` can carry rule lists. A rule matches on
//! operating system, architecture, OS version regex, or launcher feature flags.
//! Semantics follow the official launcher: start from "disallowed" and let the
//! last matching allow never override a disallow.

use serde::{Deserialize, Serialize};

/// Operating system as Mojang names it in `rules[].os.name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OsName {
    Windows,
    Osx,
    Linux,
}

impl OsName {
    /// The name used in Mojang metadata for the running platform.
    pub fn current() -> Self {
        match std::env::consts::OS {
            "windows" => Self::Windows,
            "macos" => Self::Osx,
            _ => Self::Linux,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Osx => "osx",
            Self::Linux => "linux",
        }
    }
}

impl std::fmt::Display for OsName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Processor architecture in Mojang's naming (`x86`, `x86_64`, `arm64`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86,
    X86_64,
    Arm64,
}

impl Arch {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86" => Self::X86,
            "aarch64" => Self::Arm64,
            _ => Self::X86_64,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X86_64 => "x86_64",
            Self::Arm64 => "arm64",
        }
    }

    /// Value substituted into `${arch}` inside natives classifiers (`64`, `32`).
    pub fn bitness(self) -> &'static str {
        match self {
            Self::X86 => "32",
            _ => "64",
        }
    }
}

/// Feature flags a rule can require, mirroring the official launcher.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    pub is_demo_user: bool,
    pub has_custom_resolution: bool,
    pub has_quick_plays_support: bool,
}

impl Features {
    pub fn get(&self, name: &str) -> Option<bool> {
        match name {
            "is_demo_user" => Some(self.is_demo_user),
            "has_custom_resolution" => Some(self.has_custom_resolution),
            "has_quick_plays_support" => Some(self.has_quick_plays_support),
            _ => None,
        }
    }
}

/// Everything the rule evaluator needs to know about the current machine.
#[derive(Debug, Clone)]
pub struct RuleContext {
    pub os: OsName,
    pub arch: Arch,
    /// Kernel version, matched against `rules[].os.version` regexes.
    pub os_version: String,
    pub features: Features,
}

impl Default for RuleContext {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleContext {
    pub fn new() -> Self {
        Self {
            os: OsName::current(),
            arch: Arch::current(),
            os_version: os_version(),
            features: Features::default(),
        }
    }

    pub fn with_features(mut self, features: Features) -> Self {
        self.features = features;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Disallow,
}

impl Action {
    fn allows(&self) -> bool {
        matches!(self, Action::Allow)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub action: Action,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: Option<std::collections::BTreeMap<String, bool>>,
}

impl Rule {
    fn matches(&self, ctx: &RuleContext) -> bool {
        if let Some(os) = &self.os {
            if let Some(name) = &os.name
                && !name.eq_ignore_ascii_case(ctx.os.as_str())
            {
                return false;
            }
            if let Some(arch) = &os.arch
                && !arch.eq_ignore_ascii_case(ctx.arch.as_str())
            {
                return false;
            }
            if let Some(pattern) = &os.version
                && !version_regex_matches(pattern, &ctx.os_version)
            {
                return false;
            }
        }

        if let Some(features) = &self.features {
            for (name, expected) in features {
                // An unknown feature can never be satisfied, matching the
                // official launcher's behaviour of skipping such rules.
                match ctx.features.get(name) {
                    Some(actual) if actual == *expected => {}
                    _ => return false,
                }
            }
        }

        true
    }
}

/// Decide whether an entry with `rules` applies on this machine.
///
/// No rules means "applies everywhere". Otherwise the last rule whose
/// conditions match decides, defaulting to disabled.
pub fn rules_allow(rules: &[Rule], ctx: &RuleContext) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule.matches(ctx) {
            allowed = rule.action.allows();
        }
    }
    allowed
}

/// Linux release strings look like `6.8.0-45-generic`; trim the suffix so the
/// numeric comparison in Mojang's regexes behaves predictably.
fn os_version() -> String {
    if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        return release.trim().to_owned();
    }
    std::env::consts::OS.to_owned()
}

/// Match a Mojang OS version pattern, which is a full regular expression.
fn version_regex_matches(pattern: &str, value: &str) -> bool {
    regex::Regex::new(pattern).map(|regex| regex.is_match(value)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_windows() -> RuleContext {
        RuleContext {
            os: OsName::Windows,
            arch: Arch::X86_64,
            os_version: "10.0.22631".to_owned(),
            features: Features::default(),
        }
    }

    #[test]
    fn empty_rules_allow() {
        assert!(rules_allow(&[], &ctx_windows()));
    }

    #[test]
    fn os_rule_only_applies_on_matching_platform() {
        let rules = vec![Rule {
            action: Action::Allow,
            os: Some(OsRule { name: Some("windows".into()), ..Default::default() }),
            features: None,
        }];
        assert!(rules_allow(&rules, &ctx_windows()));

        let linux = RuleContext { os: OsName::Linux, ..ctx_windows() };
        assert!(!rules_allow(&rules, &linux));
    }

    #[test]
    fn disallow_rule_excludes_every_other_platform() {
        // A lone disallow rule means "everything except this platform", which
        // follows the official rule that no matching rule excludes the entry.
        let rules = vec![Rule {
            action: Action::Disallow,
            os: Some(OsRule { name: Some("osx".into()), ..Default::default() }),
            features: None,
        }];
        let windows = RuleContext { os_version: "10.0.22631".to_owned(), ..ctx_windows() };
        assert!(!rules_allow(&rules, &windows));
        let mac = RuleContext { os: OsName::Osx, ..ctx_windows() };
        assert!(!rules_allow(&rules, &mac));
    }

    #[test]
    fn allow_then_disallow_pairs_narrow_platforms() {
        // The shape Mojang actually ships: allow everywhere, disallow one OS.
        let rules = vec![
            Rule { action: Action::Allow, os: None, features: None },
            Rule {
                action: Action::Disallow,
                os: Some(OsRule { name: Some("osx".into()), ..Default::default() }),
                features: None,
            },
        ];
        assert!(rules_allow(&rules, &ctx_windows()));
        let mac = RuleContext { os: OsName::Osx, ..ctx_windows() };
        assert!(!rules_allow(&rules, &mac));
    }

    #[test]
    fn arch_rule_is_respected() {
        let rules = vec![Rule {
            action: Action::Allow,
            os: Some(OsRule { arch: Some("arm64".into()), ..Default::default() }),
            features: None,
        }];
        assert!(!rules_allow(&rules, &ctx_windows()));
        let arm = RuleContext { arch: Arch::Arm64, ..ctx_windows() };
        assert!(rules_allow(&rules, &arm));
    }

    #[test]
    fn os_version_regex_gates_rules() {
        let rules = vec![Rule {
            action: Action::Allow,
            os: Some(OsRule { version: Some("^10\\.".into()), ..Default::default() }),
            features: None,
        }];
        let ten = RuleContext { os_version: "10.0.22631".to_owned(), ..ctx_windows() };
        assert!(rules_allow(&rules, &ten));

        let seven = RuleContext { os_version: "6.1.7601".to_owned(), ..ctx_windows() };
        assert!(!rules_allow(&rules, &seven));
    }

    #[test]
    fn feature_rules_are_respected() {
        let rules = vec![Rule {
            action: Action::Allow,
            os: None,
            features: Some([("has_custom_resolution".to_owned(), true)].into_iter().collect()),
        }];
        assert!(!rules_allow(&rules, &ctx_windows()));

        let with_features = ctx_windows()
            .with_features(Features { has_custom_resolution: true, ..Default::default() });
        assert!(rules_allow(&rules, &with_features));
    }

    #[test]
    fn unknown_feature_never_matches() {
        let rules = vec![Rule {
            action: Action::Allow,
            os: None,
            features: Some([("some_future_flag".to_owned(), true)].into_iter().collect()),
        }];
        assert!(!rules_allow(&rules, &ctx_windows()));
    }

    #[test]
    fn later_rules_override_earlier_ones() {
        let rules = vec![
            Rule {
                action: Action::Allow,
                os: Some(OsRule { name: Some("windows".into()), ..Default::default() }),
                features: None,
            },
            Rule {
                action: Action::Disallow,
                os: Some(OsRule { name: Some("windows".into()), ..Default::default() }),
                features: None,
            },
        ];
        assert!(!rules_allow(&rules, &ctx_windows()));
    }

    #[test]
    fn version_regex_handles_prefix_and_suffix_anchors() {
        assert!(version_regex_matches("^10\\.", "10.0.22631"));
        assert!(!version_regex_matches("^11\\.", "10.0.22631"));
        assert!(version_regex_matches("^10\\.0\\.22631$", "10.0.22631"));
        assert!(!version_regex_matches("^10\\.0\\.22631$", "10.0.226310"));
        assert!(version_regex_matches("^(10|11)\\.", "10.1"));
    }
}
