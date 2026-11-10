//! Turning an [`InstallPlan`] into a [`Lockfile`](crate::manifest::Lockfile).

use std::path::Path;

use crate::install::planner::{InstallPlan, PlanRoots};
use crate::instance::InstanceSpec;
use crate::manifest::lock::{
    self, FileScope, LOCKFILE_SCHEMA_VERSION, LockedFile, LockedLoader, Lockfile,
};
use crate::manifest::{LoaderSpec, ModEntry};
use crate::net::downloader::PlannedFile;

/// Build a lockfile from a resolved plan.
///
/// Paths are made relative to whichever root they live under, so the lockfile
/// survives the player moving their game directory.
pub fn from_plan(plan: &InstallPlan, instance: &InstanceSpec, roots: &PlanRoots) -> Lockfile {
    let files = plan
        .files
        .iter()
        .filter_map(|file| locked_file(&roots.common_dir, &roots.instance_dir, file))
        .collect();

    Lockfile {
        schema_version: LOCKFILE_SCHEMA_VERSION,
        server: instance.id.clone(),
        manifest_version: None,
        minecraft: instance.minecraft.clone(),
        loader: LockedLoader {
            kind: instance.loader.kind.as_str().to_owned(),
            version: instance.loader.version.clone(),
        },
        version_url: None,
        files,
        mods: Vec::new(),
    }
}

fn locked_file(common_dir: &Path, instance_dir: &Path, file: &PlannedFile) -> Option<LockedFile> {
    let (scope, relative) = match lock::relative_path(common_dir, &file.path) {
        Some(relative) => (FileScope::Common, relative),
        None => (FileScope::Instance, lock::relative_path(instance_dir, &file.path)?),
    };

    let (sha1, sha256, sha512) = match &file.hash {
        Some(hash) => match hash.algorithm {
            crate::net::hash::HashAlgorithm::Sha1 => (Some(hash.value.clone()), None, None),
            crate::net::hash::HashAlgorithm::Sha256 => (None, Some(hash.value.clone()), None),
            crate::net::hash::HashAlgorithm::Sha512 => (None, None, Some(hash.value.clone())),
            crate::net::hash::HashAlgorithm::Md5 => (None, None, None),
        },
        None => (None, None, None),
    };

    Some(LockedFile {
        kind: format!("{:?}", file.kind).to_lowercase(),
        scope,
        path: relative,
        url: file.url.clone(),
        size: file.size,
        sha1,
        sha256,
        sha512,
    })
}

/// Record the resolved loader version on an existing lockfile.
pub fn apply_loader(lockfile: &mut Lockfile, loader: &LoaderSpec, resolved_version: Option<&str>) {
    lockfile.loader = LockedLoader {
        kind: loader.kind.as_str().to_owned(),
        version: resolved_version.map(str::to_owned).or_else(|| loader.version.clone()),
    };
}

/// Record the resolved mods on an existing lockfile.
pub fn apply_mods(lockfile: &mut Lockfile, mods: &[ModEntry]) {
    lockfile.mods = mods
        .iter()
        .map(|entry| crate::manifest::LockedMod {
            source: match entry.source {
                crate::manifest::ModSource::Modrinth => "modrinth".to_owned(),
                crate::manifest::ModSource::Curseforge => "curseforge".to_owned(),
            },
            id: entry.id.clone(),
            version_id: entry.version.clone().unwrap_or_else(|| "latest".to_owned()),
            file_name: String::new(),
            sha512: None,
            required: entry.required,
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ServerEntry;
    use crate::net::downloader::{FileHash, FileKind};
    use std::path::PathBuf;

    fn instance() -> InstanceSpec {
        serde_json::from_str::<ServerEntry>(
            r#"{"id":"main","name":"Main","address":"h:25565","minecraft":"1.20.1",
                "loader":{"type":"fabric","version":"0.16.9"}}"#,
        )
        .unwrap()
        .to_instance_spec()
    }

    fn plan() -> InstallPlan {
        InstallPlan {
            files: vec![
                PlannedFile {
                    kind: FileKind::Library,
                    path: PathBuf::from("/g/common/libraries/a.jar"),
                    url: "https://example.com/a.jar".to_owned(),
                    size: 1,
                    hash: Some(FileHash::sha1("aa")),
                },
                PlannedFile {
                    kind: FileKind::Mod,
                    path: PathBuf::from("/g/instances/main/mods/sodium.jar"),
                    url: "https://example.com/sodium.jar".to_owned(),
                    size: 2,
                    hash: FileHash::sha512("bb").into(),
                },
            ],
            ..Default::default()
        }
    }

    fn roots() -> PlanRoots {
        PlanRoots::new("/g/common", "/g/instances/main")
    }

    #[test]
    fn plan_files_are_split_by_scope() {
        let lockfile = from_plan(&plan(), &instance(), &roots());
        assert_eq!(lockfile.schema_version, LOCKFILE_SCHEMA_VERSION);
        assert_eq!(lockfile.server, "main");
        assert_eq!(lockfile.loader.kind, "fabric");

        let library = lockfile.files.iter().find(|f| f.kind == "library").unwrap();
        assert_eq!(library.scope, FileScope::Common);
        assert_eq!(library.path, "libraries/a.jar");
        assert_eq!(library.sha1.as_deref(), Some("aa"));

        let modification = lockfile.files.iter().find(|f| f.kind == "mod").unwrap();
        assert_eq!(modification.scope, FileScope::Instance);
        assert_eq!(modification.path, "mods/sodium.jar");
        assert_eq!(modification.sha512.as_deref(), Some("bb"));
    }

    #[test]
    fn files_outside_every_root_are_skipped() {
        let mut plan = plan();
        plan.files.push(PlannedFile {
            kind: FileKind::Config,
            path: PathBuf::from("/elsewhere/x.cfg"),
            url: "https://example.com/x".to_owned(),
            size: 0,
            hash: None,
        });
        let lockfile = from_plan(&plan, &instance(), &roots());
        assert_eq!(lockfile.files.len(), 2);
    }

    #[test]
    fn loader_version_can_be_overridden_by_resolution() {
        let mut lockfile = from_plan(&plan(), &instance(), &roots());
        let spec = LoaderSpec {
            kind: crate::manifest::LoaderKind::Fabric,
            version: Some("0.16.9".to_owned()),
        };
        apply_loader(&mut lockfile, &spec, Some("0.16.10"));
        assert_eq!(lockfile.loader.version.as_deref(), Some("0.16.10"));

        apply_loader(&mut lockfile, &spec, None);
        assert_eq!(lockfile.loader.version.as_deref(), Some("0.16.9"));
    }

    #[test]
    fn lockfile_from_plan_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("mango-lock-plan-{}", std::process::id()));
        let mut lockfile = from_plan(&plan(), &instance(), &roots());
        lockfile.manifest_version = Some("v1".to_owned());
        lockfile.save(&dir).unwrap();

        let loaded = Lockfile::load(&dir).unwrap().unwrap();
        assert_eq!(loaded, lockfile);
        std::fs::remove_dir_all(&dir).ok();
    }
}
