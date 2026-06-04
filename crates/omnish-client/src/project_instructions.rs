use std::io::ErrorKind;
use std::path::Path;

const MAX_BYTES: usize = 128 * 1024;

/// Read `<cwd>/CLAUDE.md`, truncate at a char boundary if necessary, and
/// wrap in a `<project_instructions>` block. Returns `None` when `cwd` is
/// empty, the file is absent, or it is unreadable.
pub fn load_for_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let path = Path::new(cwd).join("CLAUDE.md");
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Err(e) => {
            crate::event_log::push(format!(
                "project_instructions: read failed at {}: {}",
                path.display(),
                e
            ));
            return None;
        }
    };
    let (body, truncated) = if content.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        (&content[..end], true)
    } else {
        (content.as_str(), false)
    };
    let tail = if truncated {
        "\n[... truncated: file exceeded 128KB ...]\n"
    } else {
        "\n"
    };
    Some(format!(
        "<project_instructions>\nSource: {}\n\n{}{}</project_instructions>",
        path.display(),
        body,
        tail
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn returns_none_for_empty_cwd() {
        assert!(load_for_cwd("").is_none());
    }

    #[test]
    fn returns_none_when_file_missing() {
        let dir = tempdir().unwrap();
        assert!(load_for_cwd(dir.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn wraps_present_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "rule one\nrule two").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.starts_with("<project_instructions>\nSource: "));
        assert!(out.contains("rule one\nrule two"));
        assert!(out.ends_with("</project_instructions>"));
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn truncates_oversized_file_at_char_boundary() {
        let dir = tempdir().unwrap();
        // Place a 3-byte CJK char straddling MAX_BYTES so the naive cut
        // would land mid-character; the boundary-finder must step back.
        let mut content = "x".repeat(MAX_BYTES - 1);
        content.push('\u{4e2d}'); // bytes [MAX_BYTES-1 .. MAX_BYTES+2)
        content.push_str(&"y".repeat(MAX_BYTES));
        fs::write(dir.path().join("CLAUDE.md"), &content).unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.contains("[... truncated: file exceeded 128KB ...]"));
        // Clean cut: the straddling CJK char must not survive.
        assert!(!out.contains('\u{4e2d}'));
    }

    #[test]
    fn includes_absolute_source_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "hello").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        let expected_path = dir.path().join("CLAUDE.md");
        assert!(out.contains(&format!("Source: {}", expected_path.display())));
    }

    #[test]
    fn returns_none_on_invalid_utf8() {
        let dir = tempdir().unwrap();
        // Bytes that are not valid UTF-8 (lone 0xFF continuation byte).
        fs::write(dir.path().join("CLAUDE.md"), [0xFFu8, 0xFE, 0xFD]).unwrap();
        assert!(load_for_cwd(dir.path().to_str().unwrap()).is_none());
    }
}
