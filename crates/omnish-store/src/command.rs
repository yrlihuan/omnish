use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub command_id: String,
    pub session_id: String,
    pub command_line: Option<String>,
    pub cwd: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output_summary: String,
    pub stream_offset: u64,
    pub stream_length: u64,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

impl CommandRecord {
    pub fn save_all(records: &[CommandRecord], dir: &Path) -> Result<()> {
        let path = dir.join("commands.json");
        let json = serde_json::to_string_pretty(records)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load_all(dir: &Path) -> Result<Vec<CommandRecord>> {
        let path = dir.join("commands.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let json = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&json)?)
    }
}
