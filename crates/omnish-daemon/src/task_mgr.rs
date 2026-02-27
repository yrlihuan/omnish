use anyhow::Result;
use std::collections::HashMap;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

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
}
