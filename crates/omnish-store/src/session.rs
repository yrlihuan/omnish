use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    #[serde(default)]
    pub attrs: HashMap<String, String>,
}

impl SessionMeta {
    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join("meta.json");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("meta.json");
        let json = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&json)?)
    }
}
