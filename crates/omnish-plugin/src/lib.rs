//! Shared plugin infrastructure: sandbox backends and tool implementations.

pub mod formatter;
pub mod sandbox;
pub mod tools;

pub use sandbox::{
    apply_in_process, bwrap_unavailable_reason, detect_backend, detect_backend_status,
    is_available, lock_policy, plugin_policy, sandbox_command, BwrapUnavailableReason,
    SandboxBackendType, SandboxDetectResult, SandboxPolicy,
};

