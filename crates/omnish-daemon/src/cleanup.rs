use crate::session_mgr::SessionManager;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub fn create_disk_cleanup_job(
    mgr: Arc<SessionManager>,
    max_age: Duration,
) -> Result<Job> {
    Ok(Job::new_async("0 0 0 * * *", move |_uuid, _lock| {
        let mgr = mgr.clone();
        Box::pin(async move {
            let cleaned = mgr.cleanup_expired_dirs(max_age).await;
            if cleaned > 0 {
                tracing::info!("cleaned up {} expired session directories", cleaned);
            }
        })
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_mgr::SessionManager;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile;

    #[test]
    fn test_create_disk_cleanup_job() {
        // Mock SessionManager
        let mock_dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(mock_dir.path().to_path_buf(), Default::default()));

        // Should create job successfully
        let job = create_disk_cleanup_job(mgr, Duration::from_secs(48 * 3600));
        assert!(job.is_ok());
    }
}