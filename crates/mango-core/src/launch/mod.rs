//! Launch argument construction, classpath assembly and process supervision.

pub mod args;
pub mod classpath;
pub mod process;
pub mod service;

pub use args::{LaunchContext, LoaderArguments, build_arguments, substitute};
pub use classpath::{ClasspathEntry, LoaderClasspath};
pub use process::{GameProcess, LaunchRequest};
pub use service::{LaunchAccount, LaunchSpec, PreparedLaunch, offline_uuid};
