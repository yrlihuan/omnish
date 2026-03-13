use ignore::types::TypesBuilder;
use ignore::WalkBuilder;
use omnish_llm::tool::ToolResult;
pub struct GrepTool;

impl GrepTool {
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

        let output_mode = input["output_mode"]
            .as_str()
            .unwrap_or("files_with_matches");

        if !matches!(output_mode, "content" | "files_with_matches" | "count") {
            return ToolResult {
                tool_use_id: String::new(),
                content: format!(
                    "Error: invalid output_mode '{}'. Expected 'content', 'files_with_matches', or 'count'",
                    output_mode
                ),
                is_error: true,
            };
        }

        let case_insensitive = input["-i"].as_bool().unwrap_or(false);
        let multiline = input["multiline"].as_bool().unwrap_or(false);

        // Build regex
        let re = {
            let mut builder = regex::RegexBuilder::new(pattern);
            builder.case_insensitive(case_insensitive);
            if multiline {
                builder.multi_line(true).dot_matches_new_line(true);
            }
            match builder.build() {
                Ok(r) => r,
                Err(e) => {
                    return ToolResult {
                        tool_use_id: String::new(),
                        content: format!("Error: invalid regex pattern: {}", e),
                        is_error: true,
                    };
                }
            }
        };

        let path = input["path"].as_str().unwrap_or(".");
        let search_path = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            match input["cwd"].as_str() {
                Some(cwd) => std::path::PathBuf::from(cwd).join(path),
                None => std::env::current_dir().unwrap_or_default().join(path),
            }
        };

        // Context options (for content mode)
        let context_before = input["-B"].as_u64().unwrap_or(0) as usize;
        let context_after = input["-A"].as_u64().unwrap_or(0) as usize;
        let context = input["-C"]
            .as_u64()
            .or(input["context"].as_u64())
            .unwrap_or(0) as usize;
        let ctx_before = if context > 0 { context } else { context_before };
        let ctx_after = if context > 0 { context } else { context_after };
        let show_line_numbers = input["-n"].as_bool().unwrap_or(true);

        // Check if searching a single file
        let is_single_file = search_path.is_file();

        // Collect matching files
        let files: Vec<std::path::PathBuf> = if is_single_file {
            vec![search_path.clone()]
        } else {
            let glob_pattern = input["glob"].as_str().unwrap_or("");
            let type_filter = input["type"].as_str().unwrap_or("");

            let mut walk = WalkBuilder::new(&search_path);
            walk.hidden(false);

            // Apply type filter
            if !type_filter.is_empty() {
                let mut types = TypesBuilder::new();
                types.add_defaults();
                types.select(type_filter);
                if let Ok(t) = types.build() {
                    walk.types(t);
                }
            }

            // Apply glob filter
            if !glob_pattern.is_empty() {
                let mut overrides = ignore::overrides::OverrideBuilder::new(&search_path);
                let _ = overrides.add(glob_pattern);
                if let Ok(o) = overrides.build() {
                    walk.overrides(o);
                }
            }

            walk.build()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
                .map(|e| e.into_path())
                .collect()
        };

        let base_path = if is_single_file {
            search_path.parent().unwrap_or(&search_path).to_path_buf()
        } else {
            search_path.clone()
        };

        // Search through files
        let mut output_lines: Vec<String> = Vec::new();

        for file_path in &files {
            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(_) => continue, // skip binary/unreadable files
            };

            let rel_path = file_path
                .strip_prefix(&base_path)
                .unwrap_or(file_path)
                .to_string_lossy();

            if multiline {
                // Multiline mode — search across the entire file content
                if re.is_match(&content) {
                    match output_mode {
                        "files_with_matches" => {
                            output_lines.push(rel_path.to_string());
                        }
                        "count" => {
                            let count = re.find_iter(&content).count();
                            output_lines.push(format!("{}:{}", rel_path, count));
                        }
                        "content" => {
                            // Show matched regions with context
                            let lines: Vec<&str> = content.lines().collect();
                            let mut matched_line_ranges = std::collections::BTreeSet::new();
                            for m in re.find_iter(&content) {
                                let start_line = content[..m.start()].matches('\n').count();
                                let end_line = content[..m.end()].matches('\n').count();
                                let from = start_line.saturating_sub(ctx_before);
                                let to = (end_line + ctx_after).min(lines.len().saturating_sub(1));
                                for l in from..=to {
                                    matched_line_ranges.insert(l);
                                }
                            }
                            if !matched_line_ranges.is_empty() {
                                for &line_idx in &matched_line_ranges {
                                    if line_idx < lines.len() {
                                        if show_line_numbers {
                                            output_lines.push(format!(
                                                "{}:{}:{}",
                                                rel_path,
                                                line_idx + 1,
                                                lines[line_idx]
                                            ));
                                        } else {
                                            output_lines.push(format!(
                                                "{}:{}",
                                                rel_path,
                                                lines[line_idx]
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                // Line-by-line mode
                let lines: Vec<&str> = content.lines().collect();
                let matching_lines: Vec<usize> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| re.is_match(line))
                    .map(|(i, _)| i)
                    .collect();

                if matching_lines.is_empty() {
                    continue;
                }

                match output_mode {
                    "files_with_matches" => {
                        output_lines.push(rel_path.to_string());
                    }
                    "count" => {
                        output_lines.push(format!("{}:{}", rel_path, matching_lines.len()));
                    }
                    "content" => {
                        // Collect line indices to show (with context)
                        let mut visible = std::collections::BTreeSet::new();
                        for &m in &matching_lines {
                            let from = m.saturating_sub(ctx_before);
                            let to = (m + ctx_after).min(lines.len().saturating_sub(1));
                            for l in from..=to {
                                visible.insert(l);
                            }
                        }
                        for &line_idx in &visible {
                            if line_idx < lines.len() {
                                if show_line_numbers {
                                    output_lines.push(format!(
                                        "{}:{}:{}",
                                        rel_path,
                                        line_idx + 1,
                                        lines[line_idx]
                                    ));
                                } else {
                                    output_lines.push(format!(
                                        "{}:{}",
                                        rel_path,
                                        lines[line_idx]
                                    ));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if output_lines.is_empty() {
            return ToolResult {
                tool_use_id: String::new(),
                content: "No matches found".to_string(),
                is_error: false,
            };
        }

        // Apply offset and head_limit
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        let head_limit = input["head_limit"].as_u64().unwrap_or(0) as usize;

        let total = output_lines.len();
        let start = offset.min(total);
        let end = if head_limit > 0 {
            (start + head_limit).min(total)
        } else {
            total
        };

        let selected = &output_lines[start..end];
        let mut result = selected.join("\n");

        if end < total {
            result.push_str(&format!(
                "\n... ({} more lines, {} total)",
                total - end,
                total
            ));
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
    use std::io::Write;

    fn make_search_tree(dir: &std::path::Path) {
        fs::create_dir_all(dir.join("src")).unwrap();
        let mut f1 = fs::File::create(dir.join("src/main.rs")).unwrap();
        writeln!(f1, "fn main() {{").unwrap();
        writeln!(f1, "    println!(\"hello world\");").unwrap();
        writeln!(f1, "}}").unwrap();

        let mut f2 = fs::File::create(dir.join("src/lib.rs")).unwrap();
        writeln!(f2, "pub fn hello() -> &'static str {{").unwrap();
        writeln!(f2, "    \"hello world\"").unwrap();
        writeln!(f2, "}}").unwrap();

        let mut f3 = fs::File::create(dir.join("readme.txt")).unwrap();
        writeln!(f3, "This is a readme file.").unwrap();
        writeln!(f3, "It contains hello world.").unwrap();
    }

    #[test]
    fn test_grep_missing_pattern() {
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({}));
        assert!(result.is_error);
        assert!(result.content.contains("pattern"));
    }

    #[test]
    fn test_grep_files_with_matches() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches"
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
        assert!(result.content.contains("lib.rs"));
        assert!(result.content.contains("readme.txt"));
    }

    #[test]
    fn test_grep_content_mode() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello world",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "content"
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("hello world"));
    }

    #[test]
    fn test_grep_count_mode() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "count"
        }));
        assert!(!result.is_error);
        // Count mode shows file:count pairs
        assert!(result.content.contains(":"));
    }

    #[test]
    fn test_grep_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "HELLO",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches",
            "-i": true
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
    }

    #[test]
    fn test_grep_case_sensitive_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "HELLO",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches"
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("No matches"));
    }

    #[test]
    fn test_grep_glob_filter() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches",
            "glob": "*.rs"
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
        assert!(!result.content.contains("readme.txt"));
    }

    #[test]
    fn test_grep_type_filter() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches",
            "type": "rust"
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
        assert!(!result.content.contains("readme.txt"));
    }

    #[test]
    fn test_grep_no_matches() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "nonexistent_string_xyz",
            "path": tmp.path().to_str().unwrap()
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("No matches"));
    }

    #[test]
    fn test_grep_head_limit() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches",
            "head_limit": 1
        }));
        assert!(!result.is_error);
        // Should show 1 file + truncation notice
        let lines: Vec<&str> = result.content.lines().collect();
        assert!(lines.len() <= 2);
        assert!(result.content.contains("more lines"));
    }

    #[test]
    fn test_grep_offset() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        // First get all matches
        let all = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches"
        }));
        let all_lines: Vec<&str> = all.content.lines().collect();
        let total = all_lines.len();

        // Now get with offset=1
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches",
            "offset": 1
        }));
        assert!(!result.is_error);
        let offset_lines: Vec<&str> = result.content.lines().collect();
        assert_eq!(offset_lines.len(), total - 1);
    }

    #[test]
    fn test_grep_invalid_output_mode() {
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "test",
            "output_mode": "invalid"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("invalid output_mode"));
    }

    #[test]
    fn test_grep_context_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = fs::File::create(tmp.path().join("test.txt")).unwrap();
        writeln!(f, "line1").unwrap();
        writeln!(f, "line2").unwrap();
        writeln!(f, "MATCH").unwrap();
        writeln!(f, "line4").unwrap();
        writeln!(f, "line5").unwrap();
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "MATCH",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "content",
            "-C": 1
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("line2"));
        assert!(result.content.contains("MATCH"));
        assert!(result.content.contains("line4"));
    }

    #[test]
    fn test_grep_multiline() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = fs::File::create(tmp.path().join("test.txt")).unwrap();
        writeln!(f, "struct Foo {{").unwrap();
        writeln!(f, "    field: i32,").unwrap();
        writeln!(f, "}}").unwrap();
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "struct Foo \\{[\\s\\S]*?field",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "content",
            "multiline": true
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("struct Foo"));
        assert!(result.content.contains("field"));
    }

    #[test]
    fn test_grep_regex() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = fs::File::create(tmp.path().join("test.txt")).unwrap();
        writeln!(f, "function hello() {{}}").unwrap();
        writeln!(f, "function world() {{}}").unwrap();
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "function\\s+\\w+",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "content"
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("function hello"));
        assert!(result.content.contains("function world"));
    }

    #[test]
    fn test_grep_default_mode_is_files() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        // No output_mode specified — should default to files_with_matches
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap()
        }));
        assert!(!result.is_error);
        // Should contain file paths, not line content with line numbers
        assert!(result.content.contains("main.rs"));
        assert!(!result.content.contains("println"));
    }

    #[test]
    fn test_grep_invalid_regex() {
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "[invalid",
            "path": "/tmp"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("invalid regex"));
    }

    #[test]
    fn test_grep_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = fs::File::create(tmp.path().join("test.txt")).unwrap();
        writeln!(f, "hello world").unwrap();
        writeln!(f, "foo bar").unwrap();
        let tool = GrepTool::new();
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().join("test.txt").to_str().unwrap(),
            "output_mode": "content"
        }));
        assert!(!result.is_error);
        assert!(result.content.contains("hello world"));
    }
}
