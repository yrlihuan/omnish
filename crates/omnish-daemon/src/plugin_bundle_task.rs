//! Issue #588: periodic rebuild of the plugin bundle as a ScheduledTask.
//!
//! The task is a baseline refresh; handlers already rebuild on demand
//! when a client's checksum disagrees with the cache. This task catches
//! slow drift even when no clients are polling, and emits an info-level
//! log only when *its own* rebuild observed a change (before vs after),
//! so handler-triggered refreshes don't show up here retroactively.

use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use omnish_common::config::ConfigMap;
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
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let bundler = bundler.clone();
            Box::pin(async move {
                tracing::debug!("task [plugin_bundle] started");
                // Compare cache before/after our own rebuild so handler-
                // triggered updates (done outside this task) don't log as
                // if this tick discovered them.
                let before = bundler.checksum();
                let after = bundler.rebuild().await;
                if after != before {
                    tracing::info!(
                        "task [plugin_bundle] changed ({} -> {})",
                        if before.is_empty() { "<empty>" } else { &before },
                        if after.is_empty() { "<empty>" } else { &after },
                    );
                }
                tracing::debug!("task [plugin_bundle] finished");
            })
        })?)
    }
}
