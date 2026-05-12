use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use omnish_common::config::ConfigMap;
use std::time::Duration;
use tokio_cron_scheduler::Job;

/// Merged housekeeping task that runs hourly and applies a user-configurable
/// retention period to both in-memory session eviction and on-disk directory
/// cleanup. Replaces the older standalone `eviction` and `disk_cleanup` tasks.
pub struct HouseKeepingTask {
    config: ConfigMap,
    schedule: String,
}

impl HouseKeepingTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = crate::task_mgr::normalize_cron(&config.get_string("schedule", ""));
        Self { config, schedule }
    }
}

/// Map the user-facing period select value to a retention duration in hours.
/// Falls back to 336h (2 weeks) for unknown values.
fn period_to_hours(period: &str) -> u64 {
    match period {
        "1 day" => 24,
        "2 days" => 48,
        "1 week" => 24 * 7,
        "2 weeks" => 24 * 14,
        "1 month" => 24 * 30,
        _ => 24 * 14,
    }
}

impl ScheduledTask for HouseKeepingTask {
    fn name(&self) -> &'static str {
        "house_keeping"
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
            ("schedule".into(), serde_json::json!("0 * * * *")),
            ("period".into(), serde_json::json!("2 weeks")),
        ]
        .into()
    }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let period = self.config.get_string("period", "2 weeks");
        let hours = period_to_hours(&period);
        let max_age = Duration::from_secs(hours * 3600);
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            Box::pin(async move {
                tracing::debug!("task [house_keeping] started (period={}h)", hours);
                let evicted = mgr.evict_inactive(max_age).await;
                let cleaned = mgr.cleanup_expired_dirs(max_age).await;
                if evicted > 0 || cleaned > 0 {
                    tracing::info!(
                        "task [house_keeping] evicted={} cleaned={}",
                        evicted,
                        cleaned
                    );
                }
                tracing::debug!("task [house_keeping] finished");
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
    fn test_period_to_hours_mapping() {
        assert_eq!(period_to_hours("1 day"), 24);
        assert_eq!(period_to_hours("2 days"), 48);
        assert_eq!(period_to_hours("1 week"), 168);
        assert_eq!(period_to_hours("2 weeks"), 336);
        assert_eq!(period_to_hours("1 month"), 720);
        // Unknown values fall back to the 2-weeks default.
        assert_eq!(period_to_hours("bogus"), 336);
        assert_eq!(period_to_hours(""), 336);
    }

    #[test]
    fn test_create_house_keeping_job() {
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
        config.set_defaults(HouseKeepingTask::defaults());
        let task = HouseKeepingTask::new(config);
        assert_eq!(task.name(), "house_keeping");
        assert!(task.enabled());
        assert_eq!(task.schedule(), "0 0 * * * *");
        let job = task.create_job(&ctx);
        assert!(job.is_ok());
    }
}
