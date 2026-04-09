//! Shared plugin infrastructure: sandbox backends and tool implementations.

pub mod formatter;
pub mod sandbox;
pub mod tools;

pub use sandbox::{
    apply_in_process, detect_backend, is_available, lock_policy, plugin_policy, sandbox_command,
    SandboxBackendType, SandboxPolicy,
};

/// Check whether Landlock is supported (used by event log / diagnostics).
pub fn is_landlock_supported() -> bool {
    is_available(SandboxBackendType::Landlock)
}

