//! Resolving launch arguments.
//!
//! Two eras exist. Before 1.13 the profile carries a single
//! `minecraftArguments` string that must be split and substituted. From 1.13 the
//! profile carries structured `arguments.game` / `arguments.jvm` arrays whose
//! entries may be gated by rules and whose placeholders look like `${name}`.
//!
//! Forge's legacy profiles keep using `minecraftArguments` even on 1.12, which
//! is why both paths must stay supported.

use std::path::Path;

use crate::error::{MangoError, Result};
use crate::mojang::rules::{RuleContext, rules_allow};
use crate::mojang::{Argument, VersionJson};

/// Values substituted into `${...}` placeholders.
#[derive(Debug, Clone, Default)]
pub struct LaunchContext {
    pub auth_player_name: String,
    pub version_name: String,
    pub game_directory: String,
    pub assets_root: String,
    pub assets_index_name: String,
    pub auth_uuid: String,
    pub auth_access_token: String,
    /// `msa` for Microsoft accounts, `mojang` otherwise.
    pub user_type: String,
    pub version_type: String,
    pub resolution_width: Option<u32>,
    pub resolution_height: Option<u32>,
    pub natives_directory: String,
    pub launcher_name: String,
    pub launcher_version: String,
    pub classpath: String,
    pub library_directory: String,
    pub classpath_separator: String,
    /// Extra placeholders contributed by loaders (`authlib-injector`, etc).
    pub extra: Vec<(String, String)>,
}

impl LaunchContext {
    /// Look up a placeholder value by its bare name (without `${}`).
    pub fn lookup(&self, name: &str) -> Option<String> {
        let value = match name {
            "auth_player_name" => self.auth_player_name.clone(),
            "version_name" => self.version_name.clone(),
            "game_directory" => self.game_directory.clone(),
            "assets_root" => self.assets_root.clone(),
            "assets_index_name" => self.assets_index_name.clone(),
            "auth_uuid" => self.auth_uuid.clone(),
            "auth_access_token" => self.auth_access_token.clone(),
            "user_type" => self.user_type.clone(),
            "version_type" => self.version_type.clone(),
            "natives_directory" => self.natives_directory.clone(),
            "launcher_name" => self.launcher_name.clone(),
            "launcher_version" => self.launcher_version.clone(),
            "classpath" => self.classpath.clone(),
            "library_directory" => self.library_directory.clone(),
            "classpath_separator" => self.classpath_separator.clone(),
            "resolution_width" => self.resolution_width.map(|w| w.to_string())?,
            "resolution_height" => self.resolution_height.map(|h| h.to_string())?,
            // Legacy profiles expect an empty JSON object here.
            "user_properties" => "{}".to_owned(),
            other => {
                self.extra.iter().find(|(key, _)| key == other).map(|(_, value)| value.clone())?
            }
        };
        Some(value)
    }
}

/// Classpath separator for the current platform.
pub fn classpath_separator() -> &'static str {
    if cfg!(windows) { ";" } else { ":" }
}

/// Replace every `${name}` in `argument`.
///
/// Unknown placeholders resolve to an empty string, matching the official
/// launcher. Dropping the whole argument instead would leave its flag dangling
/// (`--clientId` immediately followed by the next flag) and shift every
/// subsequent value.
pub fn substitute(argument: &str, ctx: &LaunchContext) -> String {
    let mut output = String::with_capacity(argument.len());
    let mut rest = argument;

    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                if let Some(value) = ctx.lookup(name) {
                    output.push_str(&value);
                }
                rest = &after[end + 1..];
            }
            None => {
                output.push_str(&rest[start..]);
                rest = "";
            }
        }
    }

    output.push_str(rest);
    output
}

/// Build the full JVM command line for a profile.
///
/// `loader_args` are appended after the profile's own arguments, which is what
/// Forge and Fabric profiles expect.
pub fn build_arguments(
    version: &VersionJson,
    ctx: &LaunchContext,
    rule_ctx: &RuleContext,
    loader_args: &LoaderArguments,
) -> Result<Vec<String>> {
    if let Some(legacy) = &version.minecraft_arguments {
        return build_legacy(version, legacy, ctx, loader_args);
    }
    build_modern(version, ctx, rule_ctx, loader_args)
}

/// Extra arguments contributed by a mod loader.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoaderArguments {
    /// Appended to the JVM section.
    pub jvm: Vec<String>,
    /// Appended after the game arguments.
    pub game: Vec<String>,
}

fn build_modern(
    version: &VersionJson,
    ctx: &LaunchContext,
    rule_ctx: &RuleContext,
    loader_args: &LoaderArguments,
) -> Result<Vec<String>> {
    let main_class = version
        .main_class
        .as_ref()
        .ok_or_else(|| MangoError::Other(format!("version {} has no mainClass", version.id)))?;

    let mut args: Vec<String> = Vec::new();

    // JVM section: profile arguments first, then the loader's.
    for argument in resolve_section(version.arguments.as_ref().map(|a| &a.jvm), rule_ctx, ctx) {
        args.push(argument);
    }
    for argument in &loader_args.jvm {
        args.push(substitute(argument, ctx));
    }

    args.push(main_class.clone());

    for argument in resolve_section(version.arguments.as_ref().map(|a| &a.game), rule_ctx, ctx) {
        args.push(argument);
    }
    for argument in &loader_args.game {
        args.push(substitute(argument, ctx));
    }

    Ok(args)
}

fn resolve_section(
    section: Option<&Vec<Argument>>,
    rule_ctx: &RuleContext,
    ctx: &LaunchContext,
) -> Vec<String> {
    let Some(arguments) = section else { return Vec::new() };
    let mut out = Vec::new();

    for argument in arguments {
        if !argument.is_allowed(rule_ctx) {
            continue;
        }
        for value in argument.values() {
            out.push(substitute(value, ctx));
        }
    }

    out
}

fn build_legacy(
    version: &VersionJson,
    minecraft_arguments: &str,
    ctx: &LaunchContext,
    loader_args: &LoaderArguments,
) -> Result<Vec<String>> {
    let main_class = version
        .main_class
        .as_ref()
        .ok_or_else(|| MangoError::Other(format!("version {} has no mainClass", version.id)))?;

    // Legacy profiles carry no `arguments.jvm`, so the classpath and natives
    // path must be supplied here. Without `-cp` the JVM cannot find the main
    // class at all, which surfaces as "Could not find or load main class".
    // Both use `=` because a JVM system property is a single argument.
    let mut args: Vec<String> = vec![
        format!("-Djava.library.path={}", ctx.natives_directory),
        "-cp".to_owned(),
        ctx.classpath.clone(),
    ];
    args.extend(loader_args.jvm.iter().map(|a| substitute(a, ctx)));
    args.push(main_class.clone());

    for token in minecraft_arguments.split_whitespace() {
        args.push(substitute(token, ctx));
    }

    // Resolution is not part of `minecraftArguments`.
    if let (Some(width), Some(height)) = (ctx.resolution_width, ctx.resolution_height) {
        args.push("--width".to_owned());
        args.push(width.to_string());
        args.push("--height".to_owned());
        args.push(height.to_string());
    }

    args.extend(loader_args.game.iter().map(|a| substitute(a, ctx)));
    Ok(args)
}

/// Shortcut used by tests and the launcher: render one rule-gated argument list.
pub fn argument_allowed(argument: &Argument, rule_ctx: &RuleContext) -> bool {
    match argument {
        Argument::Plain(_) => true,
        Argument::Conditional { rules, .. } => rules_allow(rules, rule_ctx),
    }
}

/// `true` when a path looks like a Java archive, used by classpath trimming.
pub fn ends_with_jar(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mojang::rules::{Features, OsName};

    fn ctx() -> LaunchContext {
        LaunchContext {
            auth_player_name: "Steve".to_owned(),
            version_name: "main".to_owned(),
            game_directory: "/g/instances/main".to_owned(),
            assets_root: "/g/common/assets".to_owned(),
            assets_index_name: "5".to_owned(),
            auth_uuid: "uuid-1".to_owned(),
            auth_access_token: "token-1".to_owned(),
            user_type: "msa".to_owned(),
            version_type: "release".to_owned(),
            resolution_width: Some(1920),
            resolution_height: Some(1080),
            natives_directory: "/tmp/natives".to_owned(),
            launcher_name: "MangoLauncher".to_owned(),
            launcher_version: "0.1.0".to_owned(),
            classpath: "a.jar;b.jar".to_owned(),
            library_directory: "/g/common/libraries".to_owned(),
            classpath_separator: ";".to_owned(),
            extra: Vec::new(),
        }
    }

    fn rule_ctx() -> RuleContext {
        RuleContext {
            os: OsName::Windows,
            arch: crate::mojang::Arch::X86_64,
            os_version: "10.0.22631".to_owned(),
            features: Features::default(),
        }
    }

    #[test]
    fn substitutes_known_placeholders() {
        let ctx = ctx();
        assert_eq!(substitute("${auth_player_name}", &ctx), "Steve");
        assert_eq!(
            substitute("-Djava.library.path=${natives_directory}", &ctx),
            "-Djava.library.path=/tmp/natives"
        );
        assert_eq!(substitute("${launcher_name}/${launcher_version}", &ctx), "MangoLauncher/0.1.0");
    }

    #[test]
    fn unknown_placeholders_become_empty() {
        let ctx = ctx();
        assert_eq!(substitute("${unknown_thing}", &ctx), "");
        // A flag with an unknown value keeps the flag, matching the official
        // launcher, so argument positions never shift.
        assert_eq!(substitute("--clientId ${clientid}", &ctx), "--clientId ");
    }

    #[test]
    fn version_name_is_the_server_not_the_mc_version() {
        let ctx = ctx();
        assert_eq!(substitute("${version_name}", &ctx), "main");
    }

    #[test]
    fn modern_profiles_order_jvm_then_main_then_game() {
        let version: VersionJson = serde_json::from_str(
            r#"{
                "id": "1.20.1",
                "mainClass": "net.minecraft.client.main.Main",
                "arguments": {
                    "jvm": ["-Djava.library.path=${natives_directory}", "-cp", "${classpath}"],
                    "game": ["--username", "${auth_player_name}", "--uuid", "${auth_uuid}"]
                }
            }"#,
        )
        .unwrap();

        let args =
            build_arguments(&version, &ctx(), &rule_ctx(), &LoaderArguments::default()).unwrap();
        assert_eq!(args[0], "-Djava.library.path=/tmp/natives");
        assert_eq!(args[1], "-cp");
        assert_eq!(args[2], "a.jar;b.jar");
        assert_eq!(args[3], "net.minecraft.client.main.Main");
        assert_eq!(args[4], "--username");
        assert_eq!(args[5], "Steve");
        assert_eq!(args[7], "uuid-1");
    }

    #[test]
    fn loader_arguments_are_appended_in_place() {
        let version: VersionJson = serde_json::from_str(
            r#"{"id":"1.20.1","mainClass":"Main","arguments":{"jvm":["-cp","${classpath}"],"game":["--username","${auth_player_name}"]}}"#,
        )
        .unwrap();

        let loader = LoaderArguments {
            jvm: vec!["-Dfabric.skipMcProvider=true".to_owned()],
            game: vec!["--fabric.addMods".to_owned(), "@mods.txt".to_owned()],
        };
        let args = build_arguments(&version, &ctx(), &rule_ctx(), &loader).unwrap();

        let main_index = args.iter().position(|a| a == "Main").unwrap();
        let loader_jvm = args.iter().position(|a| a == "-Dfabric.skipMcProvider=true").unwrap();
        assert!(loader_jvm < main_index);
        assert_eq!(args.last().unwrap(), "@mods.txt");
    }

    #[test]
    fn legacy_profiles_split_minecraft_arguments() {
        let version: VersionJson = serde_json::from_str(
            r#"{"id":"1.12.2","mainClass":"net.minecraft.launchwrapper.Launch",
                "minecraftArguments":"--username ${auth_player_name} --gameDir ${game_directory} --userProperties ${user_properties} --uuid ${auth_uuid}"}"#,
        )
        .unwrap();

        let args =
            build_arguments(&version, &ctx(), &rule_ctx(), &LoaderArguments::default()).unwrap();

        // Legacy profiles have no `arguments.jvm`, so the launcher must supply a
        // classpath or the JVM cannot find the main class.
        assert!(args.windows(2).any(|w| w[0] == "-cp" && w[1] == "a.jar;b.jar"));
        assert!(
            args.iter().any(|a| a == "-Djava.library.path=/tmp/natives"),
            "system properties must be one `=`-joined argument, not two"
        );

        let main_index =
            args.iter().position(|a| a == "net.minecraft.launchwrapper.Launch").unwrap();
        assert!(main_index > 0, "main class must come after the JVM arguments");

        assert!(args.windows(2).any(|w| w[0] == "--username" && w[1] == "Steve"));
        assert!(args.windows(2).any(|w| w[0] == "--gameDir" && w[1] == "/g/instances/main"));
        assert!(args.windows(2).any(|w| w[0] == "--userProperties" && w[1] == "{}"));
    }

    #[test]
    fn legacy_profiles_get_explicit_resolution() {
        let version: VersionJson = serde_json::from_str(
            r#"{"id":"1.8.9","mainClass":"net.minecraft.client.main.Main",
                "minecraftArguments":"--username ${auth_player_name}"}"#,
        )
        .unwrap();

        let args =
            build_arguments(&version, &ctx(), &rule_ctx(), &LoaderArguments::default()).unwrap();
        assert!(args.windows(2).any(|w| w[0] == "--width" && w[1] == "1920"));
        assert!(args.windows(2).any(|w| w[0] == "--height" && w[1] == "1080"));
    }

    #[test]
    fn rule_gated_arguments_are_skipped_when_not_applicable() {
        let version: VersionJson = serde_json::from_str(
            r#"{"id":"1.20.1","mainClass":"Main","arguments":{"game":[
                {"rules":[{"action":"allow","features":{"has_custom_resolution":true}}],
                 "value":["--width","${resolution_width}","--height","${resolution_height}"]}
            ]}}"#,
        )
        .unwrap();

        let args =
            build_arguments(&version, &ctx(), &rule_ctx(), &LoaderArguments::default()).unwrap();
        assert_eq!(args, vec!["Main"]);

        let with_resolution = RuleContext {
            features: Features { has_custom_resolution: true, ..Default::default() },
            ..rule_ctx()
        };
        let args = build_arguments(&version, &ctx(), &with_resolution, &LoaderArguments::default())
            .unwrap();
        assert!(args.windows(2).any(|w| w[0] == "--width" && w[1] == "1920"));
    }

    #[test]
    fn unresolved_resolution_values_become_empty_not_dropped() {
        let version: VersionJson = serde_json::from_str(
            r#"{"id":"x","mainClass":"Main","arguments":{"game":["--width","${resolution_width}"]}}"#,
        )
        .unwrap();

        let mut ctx = ctx();
        ctx.resolution_width = None;
        let args =
            build_arguments(&version, &ctx, &rule_ctx(), &LoaderArguments::default()).unwrap();
        assert_eq!(args, vec!["Main", "--width", ""]);
    }

    #[test]
    fn missing_main_class_is_an_error() {
        let version: VersionJson = serde_json::from_str(r#"{"id":"x"}"#).unwrap();
        assert!(
            build_arguments(&version, &ctx(), &rule_ctx(), &LoaderArguments::default()).is_err()
        );
    }
}
