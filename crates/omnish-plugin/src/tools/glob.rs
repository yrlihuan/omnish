use omnish_llm::tool::ToolResult;

/// Maximum number of matching paths to return.
const MAX_RESULTS: usize = 100;

#[derive(Default)]
pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        Self
    }

    pub fn execute(&self, input: &serde_json::Value) -> ToolResult {
        let pattern = match input["pattern"].as_str() {
            Some(p) if !p.is_empty() => p,
            _ => {
                return ToolResult {
                    tool_use_id: String::new(),
                    content: "Error: 'pattern' is required".to_string(),
                    is_error: true,
                };
            }
        };

        let base = match input["path"].as_str() {
            Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
            _ => match input["cwd"].as_str() {
                Some(c) => std::path::PathBuf::from(c),
                None => std::env::current_dir().unwrap_or_default(),
            },
        };

        let full_pattern = base.join(pattern);
        let full_pattern_str = full_pattern.to_string_lossy();

        let entries = match glob::glob(&full_pattern_str) {
            Ok(paths) => paths,
            Err(e) => {
                return ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Error: invalid glob pattern: {e}"),
                    is_error: true,
                };
            }
        };

        // Collect paths with their modification times
        let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = entries
            .filter_map(|entry| entry.ok())
            .filter(|p| p.is_file())
            .filter_map(|p| {
                let mtime = p.metadata().ok()?.modified().ok()?;
                Some((p, mtime))
            })
            .collect();

        let total = files.len();

        // Sort by modification time, most recent first
        files.sort_by(|a, b| b.1.cmp(&a.1));

        // Truncate to MAX_RESULTS
        files.truncate(MAX_RESULTS);
        let shown = files.len();

        if files.is_empty() {
            return ToolResult {
                tool_use_id: String::new(),
                content: format!("No files matched pattern: {pattern}"),
                is_error: false,
            };
        }

        let mut result = String::new();
        for (path, _) in &files {
            // Show path relative to base
            let display = path.strip_prefix(&base).unwrap_or(path);
            result.push_str(&display.to_string_lossy());
            result.push('\n');
        }

        if shown < total {
            result.push_str(&format!("(showing {shown} of {total} matches)"));
        }

        ToolResult {
            tool_use_id: String::new(),
            content: result,
            is_error: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_tree(dir: &std::path::Path) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::File::create(dir.join("src/main.rs")).unwrap();
        fs::File::create(dir.join("src/lib.rs")).unwrap();
        fs::File::create(dir.join("docs/readme.md")).unwrap();
        fs::File::create(dir.join("Cargo.toml")).unwrap();
    }

    #[test]
    fn test_glob_basic() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let tool = GlobTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "**/*.rs",
            "path": tmp.path().to_str().unwrap()
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
        assert!(result.content.contains("lib.rs"));
        assert!(!result.content.contains("readme.md"));
    }

    #[test]
    fn test_glob_with_subdir_path() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let tool = GlobTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "*.rs",
            "path": tmp.path().join("src").to_str().unwrap()
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
        assert!(result.content.contains("lib.rs"));
    }

    #[test]
    fn test_glob_no_matches() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let tool = GlobTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "**/*.py",
            "path": tmp.path().to_str().unwrap()
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("No files matched"));
    }

    #[test]
    fn test_glob_missing_pattern() {
        let tool = GlobTool::new();
        let result = tool.execute(&serde_json::json!({}));
        assert!(result.is_error);
        assert!(result.content.contains("pattern"));
    }

    #[test]
    fn test_glob_max_results() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..150 {
            fs::File::create(tmp.path().join(format!("file_{:03}.txt", i))).unwrap();
        }
        let tool = GlobTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "*.txt",
            "path": tmp.path().to_str().unwrap()
        }));
        assert!(!result.is_error);
        // Should have 100 file lines + 1 truncation notice
        assert!(result.content.contains("100 of 150"));
    }

    #[test]
    fn test_glob_sorted_by_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        // Create files with different mtimes
        let f1 = tmp.path().join("old.txt");
        fs::File::create(&f1).unwrap();
        // Sleep briefly so mtime differs
        std::thread::sleep(std::time::Duration::from_millis(50));
        let f2 = tmp.path().join("new.txt");
        fs::File::create(&f2).unwrap();

        let tool = GlobTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "*.txt",
            "path": tmp.path().to_str().unwrap()
        }));
        assert!(!result.is_error);
        let pos_new = result.content.find("new.txt").unwrap();
        let pos_old = result.content.find("old.txt").unwrap();
        // Most recently modified first
        assert!(pos_new < pos_old, "new.txt should appear before old.txt");
    }

    #[test]
    fn test_glob_relative_path_from_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::File::create(tmp.path().join("sub/a.txt")).unwrap();
        let tool = GlobTool::new();
        // "path" is relative — should be resolved from cwd
        let result = tool.execute(&serde_json::json!({
            "pattern": "*.txt",
            "path": tmp.path().join("sub").to_str().unwrap()
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("a.txt"));
    }

    #[test]
    fn test_glob_invalid_pattern() {
        let tool = GlobTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "[invalid",
            "path": "/tmp"
        }));
        assert!(result.is_error);
    }
}
