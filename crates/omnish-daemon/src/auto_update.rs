use anyhow::Result;
use std::path::PathBuf;
use tokio_cron_scheduler::Job;

/// Create a cron job that runs `install.sh --upgrade` to check for and install updates,
/// then runs `deploy.sh` to distribute to client machines.
pub fn create_auto_update_job(
    omnish_dir: PathBuf,
    schedule: &str,
    clients: Vec<String>,
) -> Result<Job> {
    Ok(Job::new_async(schedule, move |_uuid, _lock| {
        let omnish_dir = omnish_dir.clone();
        let clients = clients.clone();
        Box::pin(async move {
            tracing::debug!("task [auto_update] started");

            // Phase 1: Update server
            let install_script = omnish_dir.join("install.sh");
            if !install_script.exists() {
                tracing::warn!("task [auto_update] install.sh not found: {}", install_script.display());
                return;
            }

            let output = tokio::process::Command::new("bash")
                .arg(&install_script)
                .arg("--upgrade")
                .env("OMNISH_HOME", &omnish_dir)
                .output()
                .await;

            match output {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        tracing::info!("task [auto_update] {}", line);
                    }
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        tracing::warn!("task [auto_update] install.sh --upgrade failed: {}{}", stdout, stderr);
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!("task [auto_update] failed to run install.sh: {}", e);
                    return;
                }
            }

            // Phase 2: Deploy to clients
            if clients.is_empty() {
                tracing::debug!("task [auto_update] no clients configured, skipping deploy");
                return;
            }

            let deploy_script = omnish_dir.join("deploy.sh");
            if !deploy_script.exists() {
                tracing::warn!("task [auto_update] deploy.sh not found: {}", deploy_script.display());
                return;
            }

            let mut cmd = tokio::process::Command::new("bash");
            cmd.arg(&deploy_script)
                .env("OMNISH_HOME", &omnish_dir);
            for client in &clients {
                cmd.arg(client);
            }

            match cmd.output().await {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        tracing::info!("task [auto_update] {}", line);
                    }
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        tracing::warn!("task [auto_update] deploy.sh failed: {}{}", stdout, stderr);
                    }
                }
                Err(e) => {
                    tracing::warn!("task [auto_update] failed to run deploy.sh: {}", e);
                }
            }

            tracing::debug!("task [auto_update] finished");
        })
    })?)
}
