use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use omnish_common::config::ConfigMap;
use std::time::Duration;
use tokio_cron_scheduler::Job;

/// Ends sessions whose transport connection dropped without a SessionEnd and
/// whose disconnect grace period has elapsed. Pairs with the on_disconnect
/// hook in RpcServer::serve: that hook only *marks* sessions as pending-end,
/// this task does the actual end_session after the grace window so brief
/// reconnects (network blips, daemon restart) don't cause a false positive.
pub struct DisconnectSweepTask {
    config: ConfigMap,
    schedule: String,
}

/// Cron expression for the sweep (every 10 minutes). Hardcoded because this
/// task is system-level maintenance; we deliberately don't expose it in
/// daemon.toml. `normalize_cron` adds the seconds field.
const DEFAULT_SCHEDULE: &str = "*/10 * * * *";

/// Grace window between "transport connection dropped" and "end the session"
/// (1 hour). Covers brief network blips, daemon auto-update restart, laptop
/// sleep/resume.
const DEFAULT_GRACE_MINUTES: u64 = 60;

impl DisconnectSweepTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = crate::task_mgr::normalize_cron(
            &config.get_string("schedule", DEFAULT_SCHEDULE),
        );
        Self { config, schedule }
    }
}

impl ScheduledTask for DisconnectSweepTask {
    fn name(&self) -> &'static str {
        "disconnect_sweep"
    }

    fn schedule(&self) -> &str {
        &self.schedule
    }

    fn enabled(&self) -> bool {
        self.config.get_bool("enabled", true)
    }

    fn defaults() -> std::collections::HashMap<String, serde_json::Value> {
        // Returned for trait completeness; not injected into daemon.toml
        // (see task_mgr::inject_task_defaults). The actual fallbacks used
        // at runtime are the DEFAULT_* consts above.
        [
            ("enabled".into(), serde_json::json!(true)),
            ("schedule".into(), serde_json::json!(DEFAULT_SCHEDULE)),
            ("grace_minutes".into(), serde_json::json!(DEFAULT_GRACE_MINUTES)),
        ]
        .into()
    }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let minutes = self.config.get_u64("grace_minutes", DEFAULT_GRACE_MINUTES);
        let grace = Duration::from_secs(minutes * 60);
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            Box::pin(async move {
                tracing::debug!("task [disconnect_sweep] started (grace={}m)", minutes);
                let ended = mgr.sweep_disconnected(grace).await;
                if !ended.is_empty() {
                    tracing::info!(
                        "task [disconnect_sweep] ended {} session(s) after grace",
                        ended.len()
                    );
                }
                // Note: active_threads cleanup for any sessions ended here
                // is handled by the existing idle-thread loop in
                // DaemonServer::serve (30m10s safety net). Wiring it here
                // would require plumbing active_threads into TaskContext,
                // which no other task needs.
                tracing::debug!("task [disconnect_sweep] finished");
            })
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_mgr::ConversationManager;
    use crate::session_mgr::SessionManager;
    use crate::task_mgr::{DaemonContext, TaskContext};
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn test_create_disconnect_sweep_job() {
        let mock_dir = tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(
            mock_dir.path().to_path_buf(),
            Default::default(),
        ));
        let conv_mgr = Arc::new(ConversationManager::new(mock_dir.path().join("threads")));
        let llm_backend = Arc::new(std::sync::RwLock::new(Arc::new(
            omnish_llm::factory::MultiBackend::from_single(Arc::new(
                omnish_llm::backend::UnavailableBackend,
            )),
        )));
        let daemon = Arc::new(DaemonContext {
            omnish_dir: mock_dir.path().to_path_buf(),
            restart_signal: Arc::new(tokio::sync::Notify::new()),
            update_cache: Arc::new(crate::update_cache::UpdateCache::new(mock_dir.path())),
            plugin_bundler: Arc::new(crate::plugin_bundle::PluginBundler::new(
                mock_dir.path().join("plugins"),
            )),
        });
        let daemon_config = Arc::new(std::sync::RwLock::new(
            omnish_common::config::DaemonConfig::default(),
        ));
        let ctx = TaskContext {
            session_mgr: mgr,
            conv_mgr,
            llm_backend,
            daemon,
            daemon_config,
        };

        let mut config = ConfigMap::default();
        config.set_defaults(DisconnectSweepTask::defaults());
        let task = DisconnectSweepTask::new(config);
        assert!(task.enabled());
        assert_eq!(task.name(), "disconnect_sweep");
        let job = task.create_job(&ctx);
        assert!(job.is_ok());
    }
}
