//! Maven coordinates as used by Mojang library metadata.
//!
//! `group:artifact:version[:classifier][@extension]`, resolved to a
//! `group/artifact/version/artifact-version[-classifier].extension` path.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{MangoError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MavenCoordinate {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub classifier: Option<String>,
    pub extension: String,
}

impl MavenCoordinate {
    /// Parse an identifier such as `net.minecraft:launchwrapper:1.12` or
    /// `org.lwjgl:lwjgl:3.3.1:natives-windows@jar`.
    pub fn parse(id: &str) -> Result<Self> {
        let (coords, extension) = match id.rsplit_once('@') {
            Some((left, ext)) => (left, ext.to_owned()),
            None => (id, "jar".to_owned()),
        };

        let mut parts = coords.split(':');
        let group = parts.next().filter(|s| !s.is_empty());
        let artifact = parts.next().filter(|s| !s.is_empty());
        let version = parts.next().filter(|s| !s.is_empty());
        let classifier = parts.next().filter(|s| !s.is_empty());

        if parts.next().is_some() {
            return Err(MangoError::Other(format!("too many segments in maven id `{id}`")));
        }

        match (group, artifact, version) {
            (Some(group), Some(artifact), Some(version)) => Ok(Self {
                group: group.to_owned(),
                artifact: artifact.to_owned(),
                version: version.to_owned(),
                classifier: classifier.map(str::to_owned),
                extension,
            }),
            _ => Err(MangoError::Other(format!("invalid maven id `{id}`"))),
        }
    }

    /// Path relative to the libraries directory.
    pub fn to_path(&self) -> String {
        let file = match &self.classifier {
            Some(classifier) => {
                format!("{}-{}-{}.{}", self.artifact, self.version, classifier, self.extension)
            }
            None => format!("{}-{}.{}", self.artifact, self.version, self.extension),
        };
        format!("{}/{}/{}/{}", self.group.replace('.', "/"), self.artifact, self.version, file)
    }

    /// `group:artifact`, used to let server libraries override Mojang ones.
    pub fn versionless_id(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
}

impl fmt::Display for MavenCoordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)?;
        if let Some(classifier) = &self.classifier {
            write!(f, ":{classifier}")?;
        }
        if self.extension != "jar" {
            write!(f, "@{}", self.extension)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_standard_coordinate() {
        let coord = MavenCoordinate::parse("net.minecraft:launchwrapper:1.12").unwrap();
        assert_eq!(coord.to_path(), "net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar");
        assert_eq!(coord.versionless_id(), "net.minecraft:launchwrapper");
    }

    #[test]
    fn resolves_classifier_and_extension() {
        let coord = MavenCoordinate::parse("org.lwjgl:lwjgl:3.3.1:natives-windows@jar").unwrap();
        assert_eq!(coord.to_path(), "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar");
        assert_eq!(coord.classifier.as_deref(), Some("natives-windows"));
    }

    #[test]
    fn resolves_pack_xz_extension() {
        let coord =
            MavenCoordinate::parse("org.scala-lang:scala-library:2.11.1@jar.pack.xz").unwrap();
        assert_eq!(coord.extension, "jar.pack.xz");
        assert_eq!(
            coord.to_path(),
            "org/scala-lang/scala-library/2.11.1/scala-library-2.11.1.jar.pack.xz"
        );
    }

    #[test]
    fn rejects_malformed_ids() {
        assert!(MavenCoordinate::parse("onlygroup").is_err());
        assert!(MavenCoordinate::parse("a:b").is_err());
        assert!(MavenCoordinate::parse("a:b:c:d:e").is_err());
    }

    #[test]
    fn display_round_trips() {
        let id = "org.lwjgl:lwjgl:3.3.1:natives-windows";
        assert_eq!(MavenCoordinate::parse(id).unwrap().to_string(), id);
    }
}
