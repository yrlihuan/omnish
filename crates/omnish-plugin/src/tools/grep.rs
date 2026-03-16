use grep::matcher::Matcher;
use grep::regex::RegexMatcherBuilder;
use grep::searcher::sinks::Lossy;
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::types::TypesBuilder;
use ignore::WalkBuilder;
use omnish_llm::tool::ToolResult;

/// A Sink implementation that collects both matching and context lines.
struct ContentSink<'a> {
    rel_path: &'a str,
    show_line_numbers: bool,
    output_lines: &'a mut Vec<String>,
}

impl Sink for ContentSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let line = String::from_utf8_lossy(mat.bytes());
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if self.show_line_numbers {
            if let Some(line_num) = mat.line_number() {
                self.output_lines
                    .push(format!("{}:{}:{}", self.rel_path, line_num, line));
            } else {
                self.output_lines
                    .push(format!("{}:{}", self.rel_path, line));
            }
        } else {
            self.output_lines
                .push(format!("{}:{}", self.rel_path, line));
        }
        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        let line = String::from_utf8_lossy(ctx.bytes());
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if self.show_line_numbers {
            if let Some(line_num) = ctx.line_number() {
                self.output_lines
                    .push(format!("{}:{}:{}", self.rel_path, line_num, line));
            } else {
                self.output_lines
                    .push(format!("{}:{}", self.rel_path, line));
            }
        } else {
            self.output_lines
                .push(format!("{}:{}", self.rel_path, line));
        }
        Ok(true)
    }
}

#[derive(Default)]
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

        // Build matcher using grep-regex (from ripgrep)
        let matcher = {
            let mut builder = RegexMatcherBuilder::new();
            builder.case_insensitive(case_insensitive);
            if multiline {
                builder.multi_line(true).dot_matches_new_line(true);
            }
            match builder.build(pattern) {
                Ok(m) => m,
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

        // Build searcher using grep-searcher (from ripgrep)
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(0))
            .multi_line(multiline)
            .before_context(ctx_before)
            .after_context(ctx_after)
            .line_number(show_line_numbers)
            .build();

        // Check if searching a single file
        let is_single_file = search_path.is_file();

        // Collect files to search
        let files: Vec<std::path::PathBuf> = if is_single_file {
            vec![search_path.clone()]
        } else {
            let glob_pattern = input["glob"].as_str().unwrap_or("");
            let type_filter = input["type"].as_str().unwrap_or("");

            let mut walk = WalkBuilder::new(&search_path);
            walk.hidden(false);

            if !type_filter.is_empty() {
                let mut types = TypesBuilder::new();
                types.add_defaults();
                types.select(type_filter);
                if let Ok(t) = types.build() {
                    walk.types(t);
                }
            }

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

        // Search through files using grep-searcher
        let mut output_lines: Vec<String> = Vec::new();

        for file_path in &files {
            let rel_path = file_path
                .strip_prefix(&base_path)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            match output_mode {
                "files_with_matches" => {
                    // Just check if file has any match
                    let mut found = false;
                    let _ = searcher.search_path(
                        &matcher,
                        file_path,
                        Lossy(|_line_num, _line| {
                            found = true;
                            Ok(false) // stop after first match
                        }),
                    );
                    if found {
                        output_lines.push(rel_path);
                    }
                }
                "count" => {
                    let mut count = 0usize;
                    let _ = searcher.search_path(
                        &matcher,
                        file_path,
                        Lossy(|_line_num, line| {
                            // Count all pattern occurrences within each reported line
                            let _ = matcher.find_iter(line.as_bytes(), |_m| {
                                count += 1;
                                true
                            });
                            Ok(true)
                        }),
                    );
                    if count > 0 {
                        output_lines.push(format!("{}:{}", rel_path, count));
                    }
                }
                "content" => {
                    let mut sink = ContentSink {
                        rel_path: &rel_path,
                        show_line_numbers,
                        output_lines: &mut output_lines,
                    };
                    let _ = searcher.search_path(&matcher, file_path, &mut sink);
                }
                _ => {}
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
        let lines: Vec<&str> = result.content.lines().collect();
        assert!(lines.len() <= 2);
        assert!(result.content.contains("more lines"));
    }

    #[test]
    fn test_grep_offset() {
        let tmp = tempfile::tempdir().unwrap();
        make_search_tree(tmp.path());
        let tool = GrepTool::new();
        let all = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap(),
            "output_mode": "files_with_matches"
        }));
        let all_lines: Vec<&str> = all.content.lines().collect();
        let total = all_lines.len();

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
        let result = tool.execute(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.path().to_str().unwrap()
        }));
        assert!(!result.is_error);
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
