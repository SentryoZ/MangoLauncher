//! Native library extraction.
//!
//! Two eras exist. Legacy versions expect the JNI libraries extracted flat into
//! the directory named by `-Djava.library.path`. Modern versions point that
//! property at a *subdirectory* (`${natives_directory}/java`) and hand LWJGL an
//! extract path of its own, so the jars must also be on the classpath and the
//! subdirectories must exist before launch.

use std::path::{Path, PathBuf};

use crate::error::{MangoError, Result};
use crate::install::planner::NativeArchive;

/// Subdirectories modern Minecraft points its native properties at.
///
/// `version.json` uses `-Djava.library.path=${natives_directory}/java`,
/// `-Djna.tmpdir=…/jna`, `-Dorg.lwjgl.system.SharedLibraryExtractPath=…/lwjgl`
/// and `-Dio.netty.native.workdir=…/netty`. LWJGL and JNA write into those paths
/// themselves, but they must exist first or the native load fails with
/// "Failed to locate library".
pub const NATIVE_SUBDIRS: &[&str] = &["java", "jna", "lwjgl", "netty"];

/// Extract every archive in `natives` into `target`, returning the file count.
///
/// Entries matching any exclusion fragment are skipped, and directory entries
/// are never written. Nested paths are flattened, which is what the game's
/// `java.library.path` loader expects for legacy versions.
pub fn extract_natives(natives: &[NativeArchive], target: &Path) -> Result<usize> {
    std::fs::create_dir_all(target)?;
    let mut written = 0;

    for native in natives {
        written += extract_archive(&native.archive, target, &native.exclusions)?;
    }

    Ok(written)
}

/// Create the native subdirectories a modern launch expects.
///
/// Harmless for older versions, which simply ignore the unused directories.
pub fn prepare_native_dirs(target: &Path) -> Result<()> {
    for dir in NATIVE_SUBDIRS {
        std::fs::create_dir_all(target.join(dir))?;
    }
    Ok(())
}

fn extract_archive(archive: &Path, target: &Path, exclusions: &[String]) -> Result<usize> {
    let file = std::fs::File::open(archive).map_err(|err| {
        MangoError::Other(format!("could not open natives archive {}: {err}", archive.display()))
    })?;

    let mut zip = zip::ZipArchive::new(file)
        .map_err(|err| MangoError::Other(format!("{} is not a zip: {err}", archive.display())))?;

    let mut written = 0;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|err| {
            MangoError::Other(format!("bad zip entry in {}: {err}", archive.display()))
        })?;

        let name = entry.name().to_owned();
        if entry.is_dir() || is_excluded(&name, exclusions) {
            continue;
        }

        // Flatten: lwjgl ships `windows/x64/OpenAL.dll` but the JVM loads by
        // bare file name from `java.library.path`.
        let Some(file_name) = name.rsplit('/').next().filter(|n| !n.is_empty()) else {
            continue;
        };
        let destination = target.join(file_name);

        let mut output = std::fs::File::create(&destination)?;
        std::io::copy(&mut entry, &mut output)?;
        written += 1;
    }

    Ok(written)
}

fn is_excluded(name: &str, exclusions: &[String]) -> bool {
    exclusions.iter().any(|fragment| name.contains(fragment.as_str()))
}

/// Create a fresh, uniquely named natives directory for one launch.
pub fn new_natives_dir(temp_root: &Path) -> Result<PathBuf> {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let dir = temp_root.join(&suffix[..16]);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Remove every directory under `temp_root`, used on startup to clean leftovers
/// from launches that exited while detached.
pub fn purge_stale_natives(temp_root: &Path) -> Result<usize> {
    if !temp_root.exists() {
        return Ok(0);
    }

    let mut removed = 0;
    for entry in std::fs::read_dir(temp_root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => removed += 1,
                Err(err) => {
                    // A still-running game holds a lock on its DLLs; leave it.
                    tracing::debug!(path = %entry.path().display(), error = %err, "could not remove natives dir");
                }
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, body) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn extraction_flattens_and_honours_exclusions() {
        let root = std::env::temp_dir().join(format!("mango-native-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let archive = root.join("native.jar");
        make_zip(
            &archive,
            &[
                ("META-INF/MANIFEST.MF", b"manifest"),
                ("windows/x64/OpenAL.dll", b"dll"),
                ("lwjgl.dll", b"root-dll"),
                ("docs/readme.txt", b"docs"),
            ],
        );

        let target = root.join("out");
        let natives = vec![NativeArchive {
            archive: archive.clone(),
            exclusions: vec!["META-INF/".to_owned(), "docs/".to_owned()],
        }];

        let written = extract_natives(&natives, &target).unwrap();
        assert_eq!(written, 2);

        assert_eq!(std::fs::read(target.join("OpenAL.dll")).unwrap(), b"dll");
        assert_eq!(std::fs::read(target.join("lwjgl.dll")).unwrap(), b"root-dll");
        assert!(!target.join("META-INF").exists());
        assert!(!target.join("docs").exists());
        assert!(!target.join("windows").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_archive_is_an_error() {
        let root =
            std::env::temp_dir().join(format!("mango-native-missing-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let natives =
            vec![NativeArchive { archive: root.join("nope.jar"), exclusions: Vec::new() }];
        let result = extract_natives(&natives, &root.join("out"));
        assert!(result.is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stale_directories_are_purged() {
        let root = std::env::temp_dir().join(format!("mango-native-purge-{}", std::process::id()));
        std::fs::create_dir_all(root.join("abc")).unwrap();
        std::fs::create_dir_all(root.join("def")).unwrap();
        std::fs::write(root.join("keep.txt"), b"x").unwrap();

        let removed = purge_stale_natives(&root).unwrap();
        assert_eq!(removed, 2);
        assert!(root.join("keep.txt").exists());
        assert!(!root.join("abc").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn new_dirs_are_unique() {
        let root = std::env::temp_dir().join(format!("mango-native-unique-{}", std::process::id()));
        let a = new_natives_dir(&root).unwrap();
        let b = new_natives_dir(&root).unwrap();
        assert_ne!(a, b);
        assert!(a.exists() && b.exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn modern_native_subdirectories_are_created() {
        let root = std::env::temp_dir().join(format!("mango-native-sub-{}", std::process::id()));
        prepare_native_dirs(&root).unwrap();

        for dir in NATIVE_SUBDIRS {
            assert!(root.join(dir).is_dir(), "missing {dir}");
        }

        std::fs::remove_dir_all(&root).ok();
    }
}
