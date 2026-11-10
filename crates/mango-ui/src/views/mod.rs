//! Screen and panel renderers.
//!
//! Each view renders from [`UiState`] and reports intent through callbacks, so
//! the app shell owns every side effect.

pub mod instances;
pub mod landing;
pub mod logs;
pub mod settings;
pub mod sidebar;
