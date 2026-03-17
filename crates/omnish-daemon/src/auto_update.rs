use anyhow::Result;
use std::path::PathBuf;
use tokio_cron_scheduler::Job;

/// Create a cron job that runs the update.sh script to check for and install updates.
///
/// The script checks GitHub for the latest release, downloads if newer,
/// and distributes updated binaries to configured client machines.
pub fn create_auto_update_job(omnish_dir: PathBuf, schedule: &str) -> Result<Job> {
    Ok(Job::new_async(schedule, move |_uuid, _lock| {
        let script = omnish_dir.join("update.sh");
        Box::pin(async move {
            tracing::debug!("task [auto_update] started");

            if !script.exists() {
                tracing::warn!("task [auto_update] script not found: {}", script.display());
                return;
            }

            match tokio::process::Command::new("bash")
                .arg(&script)
                .env("OMNISH_HOME", script.parent().unwrap_or(&script))
                .output()
                .await
            {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if output.status.success() {
                        for line in stdout.lines() {
                            tracing::info!("task [auto_update] {}", line);
                        }
                    } else {
                        tracing::warn!(
                            "task [auto_update] script exited with {}: {}{}",
                            output.status,
                            stdout,
                            stderr,
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("task [auto_update] failed to run script: {}", e);
                }
            }

            tracing::debug!("task [auto_update] finished");
        })
    })?)
}
