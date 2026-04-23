//! Issue #588: periodic rebuild of the plugin bundle as a ScheduledTask.
//!
//! inotify on `~/.omnish/plugins/` does not see writes inside per-plugin
//! subdirectories (it is non-recursive, and recursive subdir watches race
//! on new directories). A fixed poll rebuilds the bundle, compares its
//! checksum against the previous, and logs when the bundle actually
//! changes. Because `PluginBundler::rebuild` produces deterministic bytes
//! for identical content, a no-op rebuild swaps the cached snapshot for
//! an identical copy - clients never re-download unless the checksum
//! differs.

use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use omnish_common::config::ConfigMap;
use std::sync::Mutex;
use tokio_cron_scheduler::Job;

pub struct PluginBundleTask {
    config: ConfigMap,
    schedule: String,
}

impl PluginBundleTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = crate::task_mgr::normalize_cron(&config.get_string("schedule", ""));
        Self { config, schedule }
    }
}

impl ScheduledTask for PluginBundleTask {
    fn name(&self) -> &'static str {
        "plugin_bundle"
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
        ].into()
    }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let bundler = ctx.daemon.plugin_bundler.clone();
        let prev_checksum = std::sync::Arc::new(Mutex::new(bundler.checksum()));
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let bundler = bundler.clone();
            let prev_checksum = prev_checksum.clone();
            Box::pin(async move {
                tracing::debug!("task [plugin_bundle] started");
                let checksum = bundler.rebuild().await;
                let mut prev = prev_checksum.lock().unwrap();
                if checksum != *prev {
                    tracing::info!(
                        "task [plugin_bundle] changed ({} -> {})",
                        if prev.is_empty() { "<empty>".to_string() } else { prev.clone() },
                        if checksum.is_empty() { "<empty>".to_string() } else { checksum.clone() },
                    );
                    *prev = checksum;
                }
                tracing::debug!("task [plugin_bundle] finished");
            })
        })?)
    }
}
