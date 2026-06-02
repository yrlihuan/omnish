use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

struct Logger {
    file: Option<File>,
    path: Option<String>,
    start: Instant,
}

static LOGGER: LazyLock<Mutex<Logger>> = LazyLock::new(|| {
    Mutex::new(Logger {
        file: None,
        path: None,
        start: Instant::now(),
    })
});

pub fn enable(path: &str) -> Result<(), String> {
    let mut logger = LOGGER.lock().unwrap();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("failed to open {}: {}", path, e))?;
    logger.file = Some(file);
    logger.path = Some(path.to_string());
    let elapsed = logger.start.elapsed();
    let header = format!(
        "+{:>5}.{:03} ==== debug log enabled: {} ====\n",
        elapsed.as_secs(),
        elapsed.subsec_millis(),
        path
    );
    if let Some(ref mut f) = logger.file {
        let _ = f.write_all(header.as_bytes());
        let _ = f.flush();
    }
    Ok(())
}

pub fn disable() -> Option<String> {
    let mut logger = LOGGER.lock().unwrap();
    let path = logger.path.take();
    if let Some(mut f) = logger.file.take() {
        let elapsed = logger.start.elapsed();
        let footer = format!(
            "+{:>5}.{:03} ==== debug log disabled ====\n",
            elapsed.as_secs(),
            elapsed.subsec_millis(),
        );
        let _ = f.write_all(footer.as_bytes());
        let _ = f.flush();
    }
    path
}

pub fn status() -> Option<String> {
    LOGGER.lock().unwrap().path.clone()
}

pub fn log_input(bytes: &[u8]) {
    let mut logger = LOGGER.lock().unwrap();
    if logger.file.is_none() {
        return;
    }
    let elapsed = logger.start.elapsed();
    let mut line = format!(
        "+{:>5}.{:03} [INPUT] {} ",
        elapsed.as_secs(),
        elapsed.subsec_millis(),
        bytes.len()
    );
    line.push_str(&escape_bytes(bytes));
    line.push('\n');
    if let Some(ref mut f) = logger.file {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

pub fn log_event(event: &str) {
    let mut logger = LOGGER.lock().unwrap();
    if logger.file.is_none() {
        return;
    }
    let line = format!("[EVENT] {}\n", event);
    if let Some(ref mut f) = logger.file {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        match b {
            0x1b => out.push_str("\\e"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{:02x}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_bytes() {
        assert_eq!(escape_bytes(b"abc"), "abc");
        assert_eq!(escape_bytes(b"\x1b[A"), "\\e[A");
        assert_eq!(escape_bytes(b"\n\r\t"), "\\n\\r\\t");
        assert_eq!(escape_bytes(&[0x00, 0xff]), "\\x00\\xff");
    }

    #[test]
    fn test_enable_disable_writes_to_file() {
        let tmp = std::env::temp_dir().join(format!(
            "omnish-debug-log-test-{}.log",
            std::process::id()
        ));
        let path = tmp.to_str().unwrap();
        let _ = std::fs::remove_file(&tmp);
        enable(path).expect("enable");
        log_input(b"hello\x1b");
        log_event("test-event");
        let returned = disable();
        assert_eq!(returned.as_deref(), Some(path));
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("debug log enabled"));
        assert!(content.contains("[INPUT] 6 hello\\e"));
        assert!(content.contains("[EVENT] test-event"));
        assert!(content.contains("debug log disabled"));
        let _ = std::fs::remove_file(&tmp);
    }
}
