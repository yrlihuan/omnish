//! Format-preserving TOML config file editor.
//!
//! Uses `toml_edit` to modify values in-place while keeping comments
//! and formatting intact. Commented-out lines for the same key are
//! removed to avoid confusion.

use std::path::Path;

/// A value that can be written to a TOML config file.
pub enum TomlValue {
    Bool(bool),
    String(String),
    Int(i64),
}

impl From<bool> for TomlValue {
    fn from(v: bool) -> Self {
        TomlValue::Bool(v)
    }
}

impl From<String> for TomlValue {
    fn from(v: String) -> Self {
        TomlValue::String(v)
    }
}

impl From<&str> for TomlValue {
    fn from(v: &str) -> Self {
        TomlValue::String(v.to_string())
    }
}

impl From<i64> for TomlValue {
    fn from(v: i64) -> Self {
        TomlValue::Int(v)
    }
}

/// Set a potentially nested key in a TOML file, preserving formatting.
///
/// `key` is a dot-separated path like `"llm.use_cases.completion"`.
/// Intermediate tables are created if they don't exist.
/// Creates the file if it doesn't exist.
pub fn set_toml_value_nested(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    set_toml_value_nested_inner(path, key, toml_edit::value(value))
}

/// Set a nested boolean key in a TOML file, preserving formatting.
pub fn set_toml_value_nested_bool(path: &Path, key: &str, value: bool) -> anyhow::Result<()> {
    set_toml_value_nested_inner(path, key, toml_edit::value(value))
}

/// Set a nested integer key in a TOML file, preserving formatting.
pub fn set_toml_value_nested_int(path: &Path, key: &str, value: i64) -> anyhow::Result<()> {
    set_toml_value_nested_inner(path, key, toml_edit::value(value))
}

fn set_toml_value_nested_inner(
    path: &Path,
    key: &str,
    value: toml_edit::Item,
) -> anyhow::Result<()> {
    let content = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    let mut doc = content.parse::<toml_edit::DocumentMut>()?;

    let segments: Vec<&str> = key.split('.').collect();
    if segments.len() == 1 {
        doc[segments[0]] = value;
    } else {
        let (parents, leaf) = segments.split_at(segments.len() - 1);
        let mut table = doc.as_table_mut();
        for &seg in parents {
            if !table.contains_key(seg) {
                table.insert(seg, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            table = table[seg]
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("{} is not a table", seg))?;
        }
        table[leaf[0]] = value;
    }

    let output = doc.to_string();
    let output = if output.ends_with('\n') {
        output
    } else {
        format!("{}\n", output)
    };
    std::fs::write(path, output)?;
    Ok(())
}

/// Set a top-level key in a TOML file, preserving formatting.
///
/// - Reads the file, parses with `toml_edit`, sets the key, writes back.
/// - Removes commented-out lines containing the same key name to avoid
///   stale comments like `# key = old_value` lingering after an update.
/// - Returns `Ok(())` on success, or an error if read/parse/write fails.
pub fn set_toml_value(path: &Path, key: &str, value: impl Into<TomlValue>) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let mut doc = content.parse::<toml_edit::DocumentMut>()?;

    match value.into() {
        TomlValue::Bool(v) => doc[key] = toml_edit::value(v),
        TomlValue::String(v) => doc[key] = toml_edit::value(v),
        TomlValue::Int(v) => doc[key] = toml_edit::value(v),
    }

    // Remove commented-out lines for this key
    let output = doc
        .to_string()
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with('#') && trimmed.contains(key))
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Ensure trailing newline
    let output = if output.ends_with('\n') {
        output
    } else {
        format!("{}\n", output)
    };

    std::fs::write(path, output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_set_bool_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "# some comment\nfoo = false\nbar = 42\n").unwrap();

        set_toml_value(&path, "foo", true).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("foo = true"));
        assert!(result.contains("bar = 42"));
        assert!(result.contains("# some comment"));
    }

    #[test]
    fn test_set_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "existing = 1\n").unwrap();

        set_toml_value(&path, "new_key", true).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("existing = 1"));
        assert!(result.contains("new_key = true"));
    }

    #[test]
    fn test_removes_commented_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(
            &path,
            "auto_update = true\n\n# First-run onboarding completed\n# onboarded = false\n",
        )
        .unwrap();

        set_toml_value(&path, "onboarded", true).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("onboarded = true"));
        assert!(!result.contains("# onboarded"), "commented line should be removed");
    }

    #[test]
    fn test_set_string_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "name = \"old\"\n").unwrap();

        set_toml_value(&path, "name", "new").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("name = \"new\""));
    }

    #[test]
    fn test_set_nested_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "[llm]\ndefault = \"claude\"\n").unwrap();

        set_toml_value_nested(&path, "llm.default", "openai").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("default = \"openai\""));
    }

    #[test]
    fn test_set_deeply_nested_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "").unwrap();

        set_toml_value_nested(&path, "llm.use_cases.completion", "claude-fast").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("[llm.use_cases]") || result.contains("[llm]"));
        assert!(result.contains("completion = \"claude-fast\""));
    }

    #[test]
    fn test_set_nested_creates_file_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");

        set_toml_value_nested(&path, "proxy", "http://proxy:8080").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("proxy = \"http://proxy:8080\""));
    }

    #[test]
    fn test_set_nested_bool_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "[tasks.daily_notes]\nenabled = false\n").unwrap();

        set_toml_value_nested_bool(&path, "tasks.daily_notes.enabled", true).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("enabled = true"));
    }

    #[test]
    fn test_preserves_formatting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        let content = "# omnish config\n\ndaemon_addr = \"localhost:9800\"\n\n[shell]\ncommand = \"/bin/bash\"\n";
        fs::write(&path, content).unwrap();

        set_toml_value(&path, "daemon_addr", "localhost:9900").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("# omnish config"));
        assert!(result.contains("daemon_addr = \"localhost:9900\""));
        assert!(result.contains("[shell]"));
        assert!(result.contains("command = \"/bin/bash\""));
    }
}
