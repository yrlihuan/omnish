use omnish_common::config::AutoTriggerConfig;

pub struct EventDetector {
    config: AutoTriggerConfig,
}

#[derive(Debug, Clone)]
pub enum DetectedEvent {
    PatternMatch(String),
    NonZeroExit(i32),
}

impl EventDetector {
    pub fn new(config: AutoTriggerConfig) -> Self {
        Self { config }
    }

    pub fn check_output(&self, data: &[u8]) -> Vec<DetectedEvent> {
        let text = String::from_utf8_lossy(data).to_lowercase();
        let mut events = Vec::new();
        for pattern in &self.config.on_stderr_patterns {
            if text.contains(&pattern.to_lowercase()) {
                events.push(DetectedEvent::PatternMatch(pattern.clone()));
            }
        }
        events
    }
}
