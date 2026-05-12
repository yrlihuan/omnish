use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use omnish_common::config::ConfigMap;
use std::time::Duration;
use tokio_cron_scheduler::Job;

/// Periodic safety net that closes idle stream.bin writers, releasing fds
/// when neither `SessionEnd` nor a connection-close signal fired (client
/// crash, network drop, daemon-side held connection without traffic).
pub struct WriterIdleTask {
    config: ConfigMap,
    schedule: String,
}

impl WriterIdleTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = crate::task_mgr::normalize_cron(&config.get_string("schedule", ""));
        Self { config, schedule }
    }
}

impl ScheduledTask for WriterIdleTask {
    fn name(&self) -> &'static str {
        "writer_idle"
    }

    fn schedule(&self) -> &str {
        &self.schedule
    }

    fn enabled(&self) -> bool {
        self.config.get_bool("enabled", true)
    }

    fn defaults() -> std::collections::HashMap<String, serde_json::Value> {
        [
            ("enabled".into(), serde_json::json!(true)),
            ("schedule".into(), serde_json::json!("*/5 * * * *")),
            ("idle_minutes".into(), serde_json::json!(10)),
        ]
        .into()
    }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let minutes = self.config.get_u64("idle_minutes", 10);
        let max_idle = Duration::from_secs(minutes * 60);
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            Box::pin(async move {
                tracing::debug!("task [writer_idle] started");
                let closed = mgr.close_idle_writers(max_idle).await;
                if closed > 0 {
                    tracing::info!("task [writer_idle] closed {} idle writer(s)", closed);
                }
                tracing::debug!("task [writer_idle] finished");
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
    fn test_create_writer_idle_job() {
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
        config.set_defaults(WriterIdleTask::defaults());
        let task = WriterIdleTask::new(config);
        assert!(task.enabled());
        assert_eq!(task.name(), "writer_idle");
        let job = task.create_job(&ctx);
        assert!(job.is_ok());
    }
}
