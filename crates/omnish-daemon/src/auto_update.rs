use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use omnish_common::config::ConfigMap;
use tokio_cron_scheduler::Job;

pub struct AutoUpdateTask {
    config: ConfigMap,
    schedule: String,
}

impl AutoUpdateTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = config.get_string("schedule", "0 0 4 * * *");
        Self { config, schedule }
    }
}

impl ScheduledTask for AutoUpdateTask {
    fn name(&self) -> &'static str {
        "auto_update"
    }

    fn schedule(&self) -> &str {
        &self.schedule
    }

    fn enabled(&self) -> bool {
        self.config.get_bool("enabled", false)
    }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let check_url = self.config.get_opt_string("check_url");
        let restart_signal = ctx.daemon.restart_signal.clone();
        let update_cache = ctx.daemon.update_cache.clone();
        let daemon_config = ctx.daemon_config.clone();
        Ok(Job::new_async_tz(self.schedule(), chrono::Local, move |_uuid, _lock| {
            let check_url = check_url.clone();
            let restart_signal = restart_signal.clone();
            let update_cache = update_cache.clone();
            let daemon_config = daemon_config.clone();
            Box::pin(async move {
                let (proxy, no_proxy) = {
                    let dc = daemon_config.read().unwrap();
                    (dc.proxy.http_proxy.clone(), dc.proxy.no_proxy.clone())
                };
                // Phase 0: Download packages from check_url to OMNISH_HOME/updates/
                // for daemon's own platform + all known client platforms
                if let Some(ref url) = check_url {
                    let mut platforms = update_cache.known_platforms();
                    platforms.insert((std::env::consts::OS.to_string(), std::env::consts::ARCH.to_string()));

                    if url.starts_with("http://") || url.starts_with("https://") {
                        // GitHub release API
                        let mut builder = reqwest::Client::builder();
                        if let Some(ref proxy_url) = proxy {
                            if let Ok(mut p) = reqwest::Proxy::all(proxy_url) {
                                if let Some(ref np) = no_proxy {
                                    p = p.no_proxy(reqwest::NoProxy::from_string(np));
                                }
                                builder = builder.proxy(p);
                            }
                        }
                        if let Ok(client) = builder.build() {
                            let platform_list: Vec<_> = platforms.into_iter().collect();
                            let results = update_cache.download_from_github(url, &platform_list, &client).await;
                            let mut any_updated = false;
                            for (os, arch, result) in results {
                                match result {
                                    Ok(true) => {
                                        tracing::info!("task [auto_update] cached package for {}-{}", os, arch);
                                        any_updated = true;
                                    }
                                    Ok(false) => {} // already up to date
                                    Err(e) => tracing::warn!("task [auto_update] failed to cache {}-{}: {}", os, arch, e),
                                }
                            }
                            if any_updated {
                                update_cache.scan_updates();
                            }
                        }
                    } else {
                        // Local directory
                        let source_dir = std::path::Path::new(url.as_str());
                        let mut any_updated = false;
                        for (os, arch) in &platforms {
                            match update_cache.download_from_local_dir(source_dir, os, arch) {
                                Ok(true) => {
                                    tracing::info!("task [auto_update] cached package for {}-{}", os, arch);
                                    any_updated = true;
                                }
                                Ok(false) => {} // already up to date
                                Err(e) => tracing::warn!("task [auto_update] failed to cache {}-{}: {}", os, arch, e),
                            }
                        }
                        if any_updated {
                            update_cache.scan_updates();
                        }
                    }
                }

                // Phase 1: Extract cached package and run its install.sh --upgrade
                let os = std::env::consts::OS;
                let arch = std::env::consts::ARCH;
                let cached = update_cache.cached_package(os, arch);
                if cached.is_none() {
                    tracing::debug!("task [auto_update] no cached package for {}-{}, skipping", os, arch);
                    return;
                }
                let (version, tar_gz_path) = cached.unwrap();

                // Skip if cached version is not newer than the running daemon
                if omnish_common::update::compare_versions(&version, omnish_common::VERSION)
                    != std::cmp::Ordering::Greater
                {
                    return;
                }

                tracing::info!("task [auto_update] found newer version {} > running {}, proceeding with upgrade", version, omnish_common::VERSION);

                let ver = version.clone();
                let result = tokio::task::spawn_blocking(move || {
                    omnish_common::update::extract_and_run_installer(&tar_gz_path, &ver, false)
                }).await;

                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!("task [auto_update] install failed: {}", e);
                        return;
                    }
                    Err(e) => {
                        tracing::warn!("task [auto_update] install task panicked: {}", e);
                        return;
                    }
                }

                // Server binary was updated — restart to use the new binary
                tracing::info!("task [auto_update] upgrade complete, requesting daemon restart");
                restart_signal.notify_one();
            })
        })?)
    }
}
