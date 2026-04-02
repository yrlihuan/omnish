use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub struct DiskCleanupTask(pub omnish_common::config::DiskCleanupConfig);

impl ScheduledTask for DiskCleanupTask {
    fn name(&self) -> &'static str {
        "disk_cleanup"
    }

    fn schedule(&self) -> &str {
        "0 0 */6 * * *"
    }

    fn enabled(&self) -> bool {
        self.0.enabled
    }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let max_age = Duration::from_secs(48 * 3600);
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            Box::pin(async move {
                tracing::debug!("task [disk_cleanup] started");
                let cleaned = mgr.cleanup_expired_dirs(max_age).await;
                if cleaned > 0 {
                    tracing::info!("task [disk_cleanup] cleaned {} expired session directories", cleaned);
                }
                tracing::debug!("task [disk_cleanup] finished");
            })
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_mgr::SessionManager;
    use crate::conversation_mgr::ConversationManager;
    use crate::task_mgr::{DaemonContext, TaskContext};
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn test_create_disk_cleanup_job() {
        let mock_dir = tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(mock_dir.path().to_path_buf(), Default::default()));
        let conv_mgr = Arc::new(ConversationManager::new(mock_dir.path().join("threads")));
        let llm_backend = Arc::new(std::sync::RwLock::new(Arc::new(
            omnish_llm::factory::MultiBackend::from_single(
                Arc::new(omnish_llm::backend::UnavailableBackend),
            ),
        )));
        let daemon = Arc::new(DaemonContext {
            omnish_dir: mock_dir.path().to_path_buf(),
            restart_signal: Arc::new(tokio::sync::Notify::new()),
            update_cache: Arc::new(crate::update_cache::UpdateCache::new(mock_dir.path())),
        });
        let daemon_config = Arc::new(std::sync::RwLock::new(omnish_common::config::DaemonConfig::default()));
        let ctx = TaskContext {
            session_mgr: mgr,
            conv_mgr,
            llm_backend,
            daemon,
            daemon_config,
        };

        let task = DiskCleanupTask(omnish_common::config::DiskCleanupConfig::default());
        assert!(task.enabled());
        let job = task.create_job(&ctx);
        assert!(job.is_ok());
    }
}