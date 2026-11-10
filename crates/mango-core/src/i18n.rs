//! TOML-backed string tables with a `_custom` override layer.
//!
//! Matches the Helios launcher's i18n model: `en_US.toml` provides the base
//! strings and `_custom.toml` is deep-merged on top so a fork can rebrand
//! without forking translations.

use std::path::Path;

use toml::{Table, Value};

use crate::error::Result;

/// Base language file, always loaded first.
pub const BASE_LANGUAGE: &str = "en_US";
/// Override file, loaded after the base language.
pub const CUSTOM_LANGUAGE: &str = "_custom";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lang {
    root: Table,
}

impl Lang {
    /// Load `<dir>/en_US.toml` then `<dir>/_custom.toml` (when present).
    pub fn load_dir(dir: &Path) -> Result<Self> {
        let mut lang = Lang::default();
        lang.load_file(&dir.join(format!("{BASE_LANGUAGE}.toml")))?;
        let custom = dir.join(format!("{CUSTOM_LANGUAGE}.toml"));
        if custom.exists() {
            lang.load_file(&custom)?;
        }
        Ok(lang)
    }

    pub fn load_str(&mut self, text: &str) -> Result<()> {
        let parsed: Table = toml::from_str(text)?;
        merge_table(&mut self.root, parsed);
        Ok(())
    }

    pub fn load_file(&mut self, path: &Path) -> Result<()> {
        let text = std::fs::read_to_string(path)?;
        self.load_str(&text)
    }

    /// Resolve a dotted key such as `js.auth.microsoft.error.noProfileTitle`.
    ///
    /// Returns an empty string for unknown keys, matching the legacy launcher,
    /// so a missing translation degrades instead of panicking.
    pub fn query(&self, key: &str, placeholders: &[(&str, &str)]) -> String {
        let mut current: Option<&Value> = None;
        for (index, part) in key.split('.').enumerate() {
            let table: &Table = if index == 0 {
                &self.root
            } else {
                match current {
                    Some(Value::Table(table)) => table,
                    _ => return String::new(),
                }
            };
            current = table.get(part);
        }

        let value = match current {
            Some(Value::String(text)) => text.clone(),
            _ => return String::new(),
        };
        substitute(value, placeholders)
    }

    /// Same as [`Lang::query`] but returns `None` for a missing key.
    pub fn get(&self, key: &str, placeholders: &[(&str, &str)]) -> Option<String> {
        let text = self.query(key, placeholders);
        if text.is_empty() { None } else { Some(text) }
    }
}

fn substitute(mut text: String, placeholders: &[(&str, &str)]) -> String {
    for (key, value) in placeholders {
        text = text.replace(&format!("{{{key}}}"), value);
    }
    text
}

fn merge_table(dst: &mut Table, src: Table) {
    for (key, value) in src {
        match (dst.get_mut(&key), value) {
            (Some(Value::Table(dst_table)), Value::Table(src_table)) => {
                merge_table(dst_table, src_table);
            }
            (_, value) => {
                dst.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
[ejs]
    [ejs.landing]
    play = "PLAY"
    welcome = "Welcome, {name}!"
[js]
    [js.auth]
    [js.auth.microsoft]
    [js.auth.microsoft.error]
    noProfileTitle = "No profile"
"#;

    const CUSTOM: &str = r#"
[ejs]
    [ejs.landing]
    play = "START"
"#;

    #[test]
    fn queries_nested_keys() {
        let mut lang = Lang::default();
        lang.load_str(BASE).unwrap();
        assert_eq!(lang.query("js.auth.microsoft.error.noProfileTitle", &[]), "No profile");
    }

    #[test]
    fn placeholders_are_substituted() {
        let mut lang = Lang::default();
        lang.load_str(BASE).unwrap();
        assert_eq!(lang.query("ejs.landing.welcome", &[("name", "Steve")]), "Welcome, Steve!");
    }

    #[test]
    fn custom_layer_overrides_base() {
        let mut lang = Lang::default();
        lang.load_str(BASE).unwrap();
        lang.load_str(CUSTOM).unwrap();
        assert_eq!(lang.query("ejs.landing.play", &[]), "START");
        assert_eq!(lang.query("js.auth.microsoft.error.noProfileTitle", &[]), "No profile");
    }

    #[test]
    fn unknown_keys_return_empty() {
        let mut lang = Lang::default();
        lang.load_str(BASE).unwrap();
        assert_eq!(lang.query("ejs.nope", &[]), "");
        assert_eq!(lang.get("ejs.nope", &[]), None);
    }
}
