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

    /// Incrementally reload tasks: only remove/recreate tasks whose config
    /// actually changed. Unchanged tasks keep their scheduler position so
    /// their next trigger time is preserved.
    pub async fn reload(
        &mut self,
        tasks: &[Box<dyn ScheduledTask>],
        ctx: &TaskContext,
    ) -> Result<()> {
        let mut changed = Vec::new();

        for task in tasks {
            let name = task.name();
            let want_enabled = task.enabled();
            let want_schedule = task.schedule();

            match self.tasks.get(name) {
                Some(entry) if entry.enabled == want_enabled && entry.cron == want_schedule => {
                    // Unchanged — keep existing job
                    continue;
                }
                Some(_) => {
                    // Config changed — remove old job, will re-register below
                    if let Some(entry) = self.tasks.remove(name) {
                        if entry.enabled {
                            if let Err(e) = self.scheduler.remove(&entry.uuid).await {
                                tracing::warn!("failed to remove task '{}': {}", name, e);
                            }
                        }
                    }
                    changed.push(task);
                }
                None => {
                    // New task (shouldn't happen in practice, but handle it)
                    changed.push(task);
                }
            }
        }

        if changed.is_empty() {
            tracing::debug!("task reload: no changes detected");
            return Ok(());
        }

        for task in changed {
            if task.enabled() {
                match task.create_job(ctx) {
                    Ok(job) => {
                        self.register(task.name(), task.schedule(), job).await?;
                    }
                    Err(e) => {
                        tracing::warn!("failed to create job for '{}': {}", task.name(), e);
                    }
                }
            } else {
                tracing::debug!("task '{}' is now disabled", task.name());
            }
        }

        let active = self.tasks.values().filter(|e| e.enabled).count();
        tracing::info!("task reload complete: {} active tasks", active);
        Ok(())
    }
}

pub fn create_all_tasks(config: &omnish_common::config::TasksConfig) -> Vec<Box<dyn ScheduledTask>> {
    vec![
        Box::new(crate::eviction::EvictionTask(config.eviction.clone())),
        Box::new(crate::hourly_summary::HourlySummaryTask(config.hourly_summary.clone())),
        Box::new(crate::daily_notes::DailyNotesTask(config.daily_notes.clone())),
        Box::new(crate::cleanup::DiskCleanupTask(config.disk_cleanup.clone())),
        Box::new(crate::auto_update::AutoUpdateTask(config.auto_update.clone())),
        Box::new(crate::thread_summary::ThreadSummaryTask(config.thread_summary.clone())),
    ]
}
