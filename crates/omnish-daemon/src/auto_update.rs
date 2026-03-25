use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_cron_scheduler::Job;

/// Create a cron job that runs `install.sh --upgrade` to check for and install updates,
/// then runs `deploy.sh` to distribute to client machines.
///
/// When an upgrade succeeds, `restart_signal` is notified so the daemon can
/// shut down gracefully and let systemd restart it with the new binary.
pub fn create_auto_update_job(
    omnish_dir: PathBuf,
    schedule: &str,
    clients: Vec<String>,
    check_url: Option<String>,
    restart_signal: Arc<Notify>,
    update_cache: Arc<crate::update_cache::UpdateCache>,
) -> Result<Job> {
    Ok(Job::new_async(schedule, move |_uuid, _lock| {
        let omnish_dir = omnish_dir.clone();
        let clients = clients.clone();
        let check_url = check_url.clone();
        let restart_signal = restart_signal.clone();
        let update_cache = update_cache.clone();
        Box::pin(async move {
            tracing::debug!("task [auto_update] started");

            // Phase 0: Download packages from check_url to OMNISH_HOME/updates/
            // for daemon's own platform + all known client platforms
            if let Some(ref url) = check_url {
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    let source_dir = std::path::Path::new(url.as_str());
                    let mut platforms = update_cache.known_platforms();
                    platforms.insert((std::env::consts::OS.to_string(), std::env::consts::ARCH.to_string()));
                    for (os, arch) in &platforms {
                        match update_cache.download_from_local_dir(source_dir, os, arch) {
                            Ok(true) => tracing::info!("task [auto_update] cached package for {}-{}", os, arch),
                            Ok(false) => {} // already up to date
                            Err(e) => tracing::warn!("task [auto_update] failed to cache {}-{}: {}", os, arch, e),
                        }
                    }
                    update_cache.scan_updates();
                }
            }

            // Phase 1: Update server via install.sh
            // Prefer locally cached package if available
            let install_script = omnish_dir.join("install.sh");
            if !install_script.exists() {
                tracing::warn!("task [auto_update] install.sh not found: {}", install_script.display());
                return;
            }

            let mut cmd = tokio::process::Command::new("bash");
            cmd.arg(&install_script)
                .arg("--upgrade")
                .env("OMNISH_HOME", &omnish_dir);

            let os = std::env::consts::OS;
            let arch = std::env::consts::ARCH;
            let local_cache_dir = omnish_dir.join(format!("updates/{}-{}", os, arch));
            if local_cache_dir.is_dir() {
                // Use locally cached package for install
                cmd.arg(format!("--dir={}", local_cache_dir.display()));
            } else if let Some(ref url) = check_url {
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    cmd.arg(format!("--dir={}", url));
                }
            }

            let output = cmd.output().await;

            match output {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        tracing::info!("task [auto_update] {}", line);
                    }
                    let code = output.status.code().unwrap_or(-1);
                    if code == 2 {
                        // Exit 2 = already up to date, skip deploy
                        tracing::debug!("task [auto_update] already up to date, skipping deploy");
                        return;
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

            // Phase 2: Deploy to clients (if configured)
            if !clients.is_empty() {
                let deploy_script = omnish_dir.join("deploy.sh");
                if !deploy_script.exists() {
                    tracing::warn!("task [auto_update] deploy.sh not found: {}", deploy_script.display());
                } else {
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
                }
            }

            // Server binary was updated in Phase 1 — restart to use the new binary
            tracing::info!("task [auto_update] upgrade complete, requesting daemon restart");
            restart_signal.notify_one();
        })
    })?)
}
