use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 128 * 1024;
const MAX_IMPORT_DEPTH: usize = 5;

/// Read `<cwd>/CLAUDE.md`, expand `@path` imports (Claude Code syntax) as
/// `<imported_file>` blocks, truncate to a shared 128KB budget, and wrap
/// everything in a `<project_instructions>` block. Returns `None` when `cwd`
/// is empty, the file is absent, or it is unreadable.
pub fn load_for_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    load_with_home(Path::new(cwd), home.as_deref())
}

fn load_with_home(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let path = cwd.join("CLAUDE.md");
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
    let (body, truncated) = truncate_at_boundary(&content, MAX_BYTES);
    let tail = if truncated {
        "\n[... truncated: file exceeded 128KB ...]\n"
    } else {
        "\n"
    };
    let mut out = format!(
        "<project_instructions>\nSource: {}\n\n{}{}",
        path.display(),
        body,
        tail
    );
    let mut visited = HashSet::new();
    visited.insert(visit_key(&path));
    let mut budget = MAX_BYTES.saturating_sub(body.len());
    append_imports(&mut out, body, cwd, home, 1, &mut visited, &mut budget);
    out.push_str("</project_instructions>");
    Some(out)
}

/// Cut `content` to at most `max` bytes on a char boundary.
fn truncate_at_boundary(content: &str, max: usize) -> (&str, bool) {
    if content.len() <= max {
        return (content, false);
    }
    let mut end = max;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (&content[..end], true)
}

/// Key for cycle detection: canonical path when resolvable, raw path otherwise.
fn visit_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Recursively expand `@path` imports found in `content`, appending each
/// imported file as an `<imported_file>` block. `base` is the directory of
/// the file that contains `content`; relative imports resolve against it.
/// A shared byte budget caps the total appended content.
fn append_imports(
    out: &mut String,
    content: &str,
    base: &Path,
    home: Option<&Path>,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    budget: &mut usize,
) {
    if depth > MAX_IMPORT_DEPTH {
        return;
    }
    for raw in extract_imports(content) {
        let path = match resolve_import(base, &raw, home) {
            Some(p) => p,
            None => continue,
        };
        if !visited.insert(visit_key(&path)) {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                crate::event_log::push(format!(
                    "project_instructions: import @{} skipped ({}): {}",
                    raw,
                    path.display(),
                    e
                ));
                continue;
            }
        };
        if *budget == 0 {
            crate::event_log::push(format!(
                "project_instructions: import @{} dropped (128KB budget exhausted)",
                raw
            ));
            continue;
        }
        let (body, truncated) = truncate_at_boundary(&text, *budget);
        *budget -= body.len();
        let tail = if truncated {
            "\n[... truncated: import budget exceeded ...]\n"
        } else {
            "\n"
        };
        out.push_str(&format!(
            "<imported_file path=\"{}\">\n{}{}</imported_file>\n",
            path.display(),
            body,
            tail
        ));
        if let Some(parent) = path.parent() {
            append_imports(out, body, parent, home, depth + 1, visited, budget);
        }
    }
}

/// Find `@path` import tokens in markdown, skipping fenced code blocks and
/// inline code spans. A token counts as an import when `@` starts a
/// whitespace-delimited word (so `user@host` is ignored) and the path looks
/// file-like (contains `/` or `.`).
fn extract_imports(content: &str) -> Vec<String> {
    let mut imports = Vec::new();
    let mut in_fence = false;
    for line in content.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let cleaned = blank_code_spans(line);
        for token in cleaned.split_whitespace() {
            if let Some(rest) = token.strip_prefix('@') {
                if !rest.is_empty() && (rest.contains('/') || rest.contains('.')) {
                    imports.push(rest.to_string());
                }
            }
        }
    }
    imports
}

/// Replace inline-code span contents (between backticks) with spaces so the
/// token scan never sees them.
fn blank_code_spans(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_span = false;
    for c in line.chars() {
        if c == '`' {
            in_span = !in_span;
            out.push(' ');
        } else {
            out.push(if in_span { ' ' } else { c });
        }
    }
    out
}

/// Resolve an import path: absolute as-is, `~/` against `home`, anything
/// else against the importing file's directory.
fn resolve_import(base: &Path, raw: &str, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.map(|h| h.join(rest));
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        Some(p.to_path_buf())
    } else {
        Some(base.join(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn expand_for_test(cwd: &Path, home: Option<&Path>) -> String {
        load_with_home(cwd, home).unwrap()
    }

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

    // ── @ import expansion (#638) ─────────────────────────────────────────

    #[test]
    fn expands_at_import_as_imported_file_block() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("docs")).unwrap();
        fs::write(dir.path().join("docs/extra.md"), "imported rules").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "main rules\n@docs/extra.md\n").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        let expected = dir.path().join("docs/extra.md");
        assert!(out.contains(&format!("<imported_file path=\"{}\">", expected.display())));
        assert!(out.contains("imported rules"));
        // Imported blocks live inside the project_instructions wrapper.
        assert!(out.ends_with("</project_instructions>"));
    }

    #[test]
    fn expands_absolute_path_import() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("abs.md");
        fs::write(&target, "absolute content").unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            format!("see\n@{}\n", target.display()),
        )
        .unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.contains("absolute content"));
    }

    #[test]
    fn skips_import_inside_fenced_code_block() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("extra.md"), "SHOULD_NOT_APPEAR").unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "example:\n```\n@extra.md\n```\n",
        )
        .unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(!out.contains("SHOULD_NOT_APPEAR"));
        assert!(!out.contains("<imported_file"));
    }

    #[test]
    fn skips_import_inside_inline_code_span() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("extra.md"), "SHOULD_NOT_APPEAR").unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "use `@extra.md` to import\n",
        )
        .unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(!out.contains("SHOULD_NOT_APPEAR"));
    }

    #[test]
    fn ignores_email_like_tokens() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "contact user@example.com for access\n",
        )
        .unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(!out.contains("<imported_file"));
    }

    #[test]
    fn missing_import_target_is_skipped() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "main\n@docs/nope.md\n").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.contains("main"));
        assert!(!out.contains("<imported_file"));
    }

    #[test]
    fn nested_imports_resolve_relative_to_importing_file() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("docs/sub")).unwrap();
        fs::write(dir.path().join("docs/a.md"), "level one\n@sub/b.md\n").unwrap();
        fs::write(dir.path().join("docs/sub/b.md"), "deep rules").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "@docs/a.md\n").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.contains("level one"));
        assert!(out.contains("deep rules"));
    }

    #[test]
    fn cyclic_imports_are_included_once() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "alpha body\n@b.md\n").unwrap();
        fs::write(dir.path().join("b.md"), "beta body\n@a.md\n").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "@a.md\n").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(out.matches("alpha body").count(), 1);
        assert_eq!(out.matches("beta body").count(), 1);
    }

    #[test]
    fn root_file_is_never_reimported() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "alpha body\n@CLAUDE.md\n").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "root body\n@a.md\n").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(out.matches("root body").count(), 1);
    }

    #[test]
    fn import_depth_limited_to_five() {
        let dir = tempdir().unwrap();
        for i in 1..=6 {
            let next = if i < 6 {
                format!("\n@d{}.md\n", i + 1)
            } else {
                String::new()
            };
            fs::write(
                dir.path().join(format!("d{i}.md")),
                format!("LEVEL{i}MARK{next}"),
            )
            .unwrap();
        }
        fs::write(dir.path().join("CLAUDE.md"), "@d1.md\n").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.contains("LEVEL5MARK"));
        assert!(!out.contains("LEVEL6MARK"));
    }

    #[test]
    fn imports_share_total_byte_budget() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("big.md"), "z".repeat(MAX_BYTES * 2)).unwrap();
        fs::write(dir.path().join("after.md"), "AFTER_BUDGET_MARK").unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "main\n@big.md\n@after.md\n",
        )
        .unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.contains("[... truncated: import budget exceeded ...]"));
        // Budget exhausted by big.md, so after.md is dropped entirely.
        assert!(!out.contains("AFTER_BUDGET_MARK"));
        // Overall output stays near the cap (markers/wrappers add slack).
        assert!(out.len() < MAX_BYTES + 4096);
    }

    #[test]
    fn tilde_import_resolves_against_home() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        fs::write(home.path().join("home_rules.md"), "home sweet rules").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "@~/home_rules.md\n").unwrap();
        let out = expand_for_test(dir.path(), Some(home.path()));
        assert!(out.contains("home sweet rules"));
    }
}
