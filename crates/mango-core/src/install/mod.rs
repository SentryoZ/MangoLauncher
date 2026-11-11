//! Installation planning and execution.

pub mod lockfile;
pub mod natives;
pub mod planner;
pub mod service;

pub use lockfile::{apply_loader, apply_mods};
pub use natives::{
    NATIVE_SUBDIRS, extract_natives, new_natives_dir, prepare_native_dirs, purge_stale_natives,
};
pub use planner::{InstallPlan, NativeArchive, PlanRoots, add_manifest_files, plan_vanilla};
pub use service::{
    EffectiveVersion, InstallOptions, InstallOutcome, ResolvedVersion, install_instance,
    install_modpack, install_vanilla, resolve, resolve_effective_version, resolve_version_url,
};
