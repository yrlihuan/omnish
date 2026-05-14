use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

use crate::conversation_mgr::ConversationManager;
use crate::session_mgr::SessionManager;
use crate::update_cache::UpdateCache;
use omnish_llm::factory::SharedLlmBackend;

/// Shared daemon-level context (non-per-request state).
pub struct DaemonContext {
    pub omnish_dir: PathBuf,
    pub restart_signal: Arc<tokio::sync::Notify>,
    pub update_cache: Arc<UpdateCache>,
    pub plugin_bundler: Arc<crate::plugin_bundle::PluginBundler>,
}

/// Everything a scheduled task needs to build its job closure.
pub struct TaskContext {
    pub session_mgr: Arc<SessionManager>,
    pub conv_mgr: Arc<ConversationManager>,
    pub llm_backend: SharedLlmBackend,
    pub daemon: Arc<DaemonContext>,
    pub daemon_config: Arc<std::sync::RwLock<omnish_common::config::DaemonConfig>>,
}

/// Trait for self-describing, config-driven scheduled tasks.
pub trait ScheduledTask: Send + Sync {
    /// Human-readable task name (used as key in TaskManager).
    fn name(&self) -> &'static str;
    /// Cron expression for the schedule.
    fn schedule(&self) -> &str;
    /// Whether the task is enabled (from config).
    fn enabled(&self) -> bool;
    /// Build the tokio-cron-scheduler Job using the shared context.
    fn create_job(&self, ctx: &TaskContext) -> Result<Job>;
    /// Default config values for this task (injected into ConfigMap.defaults).
    fn defaults() -> HashMap<String, serde_json::Value> where Self: Sized;
}

struct TaskEntry {
    uuid: Uuid,
    cron: String,
    enabled: bool,
}

pub struct TaskManager {
    scheduler: JobScheduler,
    tasks: HashMap<String, TaskEntry>,
}

impl TaskManager {
    pub async fn new() -> Result<Self> {
        let scheduler = JobScheduler::new().await?;
        Ok(Self {
            scheduler,
            tasks: HashMap::new(),
        })
    }

    pub async fn register(&mut self, name: &str, cron: &str, job: Job) -> Result<()> {
        let uuid = self.scheduler.add(job).await?;
        self.tasks.insert(name.to_string(), TaskEntry {
            uuid,
            cron: cron.to_string(),
            enabled: true,
        });
        tracing::info!("registered task '{}' with schedule '{}'", name, cron);
        Ok(())
    }

    pub async fn start(&self) -> Result<()> {
        self.scheduler.start().await?;
        Ok(())
    }

    pub fn list(&self) -> Vec<(String, String, bool)> {
        self.tasks
            .iter()
            .map(|(name, entry)| (name.clone(), entry.cron.clone(), entry.enabled))
            .collect()
    }

    pub async fn disable(&mut self, name: &str) -> Result<()> {
        let entry = self.tasks.get_mut(name)
            .ok_or_else(|| anyhow::anyhow!("task '{}' not found", name))?;
        if !entry.enabled {
            return Ok(());
        }
        self.scheduler.remove(&entry.uuid).await?;
        entry.enabled = false;
        tracing::info!("disabled task '{}'", name);
        Ok(())
    }

    pub fn format_list(&self) -> String {
        if self.tasks.is_empty() {
            return "No scheduled tasks.".to_string();
        }
        let mut lines = vec!["Scheduled tasks:".to_string()];
        let mut entries: Vec<_> = self.tasks.iter().collect();
        entries.sort_by_key(|(name, _)| (*name).clone());
        for (name, entry) in entries {
            let status = if entry.enabled { "enabled" } else { "disabled" };
            lines.push(format!("  {} [{}] ({})", name, entry.cron, status));
        }
        lines.join("\n")
    }

    /// Reload all tasks unconditionally: remove every registered job and
    /// re-register from the supplied task list. Captures the full set of
    /// config fields (period, idle_minutes, check_url, prompts, etc.) that
    /// would otherwise be invisible to a diff based on enabled+schedule
    /// alone.
    ///
    /// Cron expressions schedule by wall-clock time, so re-registering does
    /// not shift trigger times; config changes are infrequent enough that
    /// rebuilding the small number of tasks is cheaper than maintaining a
    /// per-field fingerprint.
    pub async fn reload(
        &mut self,
        tasks: &[Box<dyn ScheduledTask>],
        ctx: &TaskContext,
    ) -> Result<()> {
        let prev_uuids: Vec<Uuid> = self.tasks.values().map(|e| e.uuid).collect();
        for uuid in prev_uuids {
            if let Err(e) = self.scheduler.remove(&uuid).await {
                tracing::warn!("failed to remove task during reload: {}", e);
            }
        }
        self.tasks.clear();

        for task in tasks {
            if !task.enabled() {
                tracing::debug!("task '{}' is disabled, skipping", task.name());
                continue;
            }
            match task.create_job(ctx) {
                Ok(job) => {
                    self.register(task.name(), task.schedule(), job).await?;
                }
                Err(e) => {
                    tracing::warn!("failed to create job for '{}': {}", task.name(), e);
                }
            }
        }

        tracing::info!("task reload complete: {} active tasks", self.tasks.len());
        Ok(())
    }
}

/// Normalize a cron expression to tokio-cron-scheduler's 6/7-field format.
/// If the input has 5 fields (standard Linux cron: min hour dom month dow),
/// prepend "0 " to add a seconds field. If 6+ fields, pass through as-is.
pub fn normalize_cron(expr: &str) -> String {
    let fields = expr.split_whitespace().count();
    if fields == 5 {
        format!("0 {}", expr)
    } else {
        expr.to_string()
    }
}

pub fn create_all_tasks(config: &omnish_common::config::TasksConfig) -> Vec<Box<dyn ScheduledTask>> {
    let empty = omnish_common::config::ConfigMap::default();
    vec![
        Box::new(crate::house_keeping::HouseKeepingTask::new(config.get("house_keeping").unwrap_or(&empty).clone())),
        Box::new(crate::hourly_summary::HourlySummaryTask::new(config.get("hourly_summary").unwrap_or(&empty).clone())),
        Box::new(crate::daily_notes::DailyNotesTask::new(config.get("daily_notes").unwrap_or(&empty).clone())),
        Box::new(crate::auto_update::AutoUpdateTask::new(config.get("auto_update").unwrap_or(&empty).clone())),
        Box::new(crate::thread_summary::ThreadSummaryTask::new(config.get("thread_summary").unwrap_or(&empty).clone())),
        Box::new(crate::plugin_bundle_task::PluginBundleTask::new(config.get("plugin_bundle").unwrap_or(&empty).clone())),
        Box::new(crate::writer_idle::WriterIdleTask::new(config.get("writer_idle").unwrap_or(&empty).clone())),
        Box::new(crate::disconnect_sweep::DisconnectSweepTask::new(config.get("disconnect_sweep").unwrap_or(&empty).clone())),
    ]
}

/// Inject task defaults into a TasksConfig so serialization includes them.
/// Called at startup and on every config reload.
pub fn inject_task_defaults(tasks: &mut omnish_common::config::TasksConfig) {
    let all_defaults: Vec<(&str, HashMap<String, serde_json::Value>)> = vec![
        ("house_keeping", crate::house_keeping::HouseKeepingTask::defaults()),
        ("hourly_summary", crate::hourly_summary::HourlySummaryTask::defaults()),
        ("daily_notes", crate::daily_notes::DailyNotesTask::defaults()),
        ("auto_update", crate::auto_update::AutoUpdateTask::defaults()),
        ("thread_summary", crate::thread_summary::ThreadSummaryTask::defaults()),
        ("plugin_bundle", crate::plugin_bundle_task::PluginBundleTask::defaults()),
        ("writer_idle", crate::writer_idle::WriterIdleTask::defaults()),
        // disconnect_sweep is a system-level maintenance task with no
        // intended user configuration; its defaults are hardcoded in the
        // task itself and not surfaced in daemon.toml.
    ];
    for (name, defaults) in all_defaults {
        let entry = tasks.entry(name.to_string()).or_default();
        entry.set_defaults(defaults);
    }
}
