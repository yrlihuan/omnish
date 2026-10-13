pub mod auth;
pub mod config;
pub mod config_edit;
pub mod host;
pub mod plugin_bundle;
pub mod sandbox_rule;
pub mod tool_image;
pub mod update;

pub const VERSION: &str = env!("OMNISH_VERSION");
