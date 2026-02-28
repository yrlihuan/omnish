use crate::session_mgr::SessionManager;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio_cron_scheduler::Job;

/// Create a cron job that cleans up expired session directories.
///
/// The job runs according to the provided cron schedule and removes session directories
/// older than `max_age`.
///
/// Returns a `tokio_cron_scheduler::Job` that can be registered with a task manager.
pub fn create_disk_cleanup_job(
    mgr: Arc<SessionManager>,
    max_age: Duration,
    schedule: &str,
) -> Result<Job> {
    Ok(Job::new_async(schedule, move |_uuid, _lock| {
        let mgr = mgr.clone();
        Box::pin(async move {
            let cleaned = mgr.cleanup_expired_dirs(max_age).await;
            if cleaned > 0 {
                tracing::info!("cleaned up {} expired session directories", cleaned);
            } else {
                tracing::debug!("no expired session directories to clean up");
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
    use tempfile::tempdir;

    #[test]
    fn test_create_disk_cleanup_job() {
        // Mock SessionManager
        let mock_dir = tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(mock_dir.path().to_path_buf(), Default::default()));

        // Should create job successfully
        let job = create_disk_cleanup_job(mgr, Duration::from_secs(48 * 3600), "0 0 0 * * *");
        assert!(job.is_ok());
    }
}