use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

const CAPACITY: usize = 200;

struct EventLog {
    events: VecDeque<String>,
    start: Instant,
}

static LOG: LazyLock<Mutex<EventLog>> = LazyLock::new(|| {
    Mutex::new(EventLog {
        events: VecDeque::new(),
        start: Instant::now(),
    })
});

/// Record an event with an automatic elapsed-time prefix.
pub fn push(event: impl std::fmt::Display) {
    let mut log = LOG.lock().unwrap();
    if log.events.len() >= CAPACITY {
        log.events.pop_front();
    }
    let elapsed = log.start.elapsed();
    let secs = elapsed.as_secs();
    let millis = elapsed.subsec_millis();
    log.events
        .push_back(format!("+{secs:>5}.{millis:03} {event}"));
}

/// Return the last `n` events (oldest first).
pub fn recent(n: usize) -> Vec<String> {
    let log = LOG.lock().unwrap();
    let skip = log.events.len().saturating_sub(n);
    log.events.iter().skip(skip).cloned().collect()
}
