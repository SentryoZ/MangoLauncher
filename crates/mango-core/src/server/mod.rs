//! Server queries.

pub mod status;

pub use status::{ServerStatus, ping, split_address, strip_formatting};
