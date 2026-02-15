use anyhow::Result;
use async_trait::async_trait;
use omnish_store::command::CommandRecord;
use omnish_store::stream::StreamEntry;

/// Reads stream entries for a given command's byte range.
pub trait StreamReader: Send + Sync {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>>;
}

/// Strategy for assembling LLM context from command history.
#[async_trait]
pub trait ContextStrategy: Send + Sync {
    async fn build_context(
        &self,
        commands: &[CommandRecord],
        reader: &dyn StreamReader,
    ) -> Result<String>;
}
