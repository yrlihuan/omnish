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
/// Segments containing dots can be quoted: `"llm.backends.\"gemini-3.1\".model"`.
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

/// Split a dot-separated key path into segments, respecting quoted segments.
/// e.g. `llm.backends."gemini-3.1".model` → `["llm", "backends", "gemini-3.1", "model"]`
pub fn split_key_path(key: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in key.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '.' if !in_quotes => {
                if !current.is_empty() {
                    segments.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Set a nested key inside an already-parsed `DocumentMut`.
/// Intermediate tables are created as needed.
pub fn set_toml_nested_in_doc(
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    value: toml_edit::Item,
) -> anyhow::Result<()> {
    let segments = split_key_path(key);
    if segments.len() == 1 {
        doc[&segments[0]] = value;
    } else {
        let (parents, leaf) = segments.split_at(segments.len() - 1);
        let mut table = doc.as_table_mut();
        for seg in parents {
            if !table.contains_key(seg) {
                table.insert(seg, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            table = table[seg.as_str()]
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("{} is not a table", seg))?;
        }
        table[&leaf[0]] = value;
    }
    Ok(())
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

    set_toml_nested_in_doc(&mut doc, key, value)?;

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

// ── TOML array operations (file-locked) ──────────────────────────────────────

/// Navigate to a nested table by dot-separated key path, creating intermediate
/// tables as needed. Returns a mutable reference to the leaf table item (array).
fn get_or_create_array<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    key: &str,
) -> anyhow::Result<&'a mut toml_edit::Array> {
    let segments = split_key_path(key);
    if segments.is_empty() {
        anyhow::bail!("empty key");
    }
    let (parents, leaf_slice) = segments.split_at(segments.len() - 1);
    let leaf = &leaf_slice[0];

    let mut table = doc.as_table_mut();
    for seg in parents {
        if !table.contains_key(seg.as_str()) {
            table.insert(seg, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        table = table[seg.as_str()]
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("{} is not a table", seg))?;
    }

    // Create array if it doesn't exist
    if !table.contains_key(leaf.as_str()) {
        table.insert(leaf, toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new())));
    }
    table[leaf.as_str()]
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("{} is not an array", leaf))
}

fn with_locked_doc<F>(path: &Path, f: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut toml_edit::DocumentMut) -> anyhow::Result<()>,
{
    use fs2::FileExt;
    let lock_path = path.with_extension("toml.lock");
    let lock_file = std::fs::File::create(&lock_path)?;
    lock_file.lock_exclusive()?;

    let content = if path.exists() { std::fs::read_to_string(path)? } else { String::new() };
    let mut doc = content.parse::<toml_edit::DocumentMut>()?;
    f(&mut doc)?;
    let output = doc.to_string();
    let output = if output.ends_with('\n') { output } else { format!("{}\n", output) };
    std::fs::write(path, output)?;
    Ok(())
}

/// Append a string value to a TOML array at the given dot-separated key path.
pub fn append_to_toml_array(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    with_locked_doc(path, |doc| {
        let arr = get_or_create_array(doc, key)?;
        arr.push(value);
        Ok(())
    })
}

/// Remove the element at `index` from a TOML array at the given key path.
pub fn remove_from_toml_array(path: &Path, key: &str, index: usize) -> anyhow::Result<()> {
    with_locked_doc(path, |doc| {
        let arr = get_or_create_array(doc, key)?;
        if index >= arr.len() {
            anyhow::bail!("index {} out of bounds (len={})", index, arr.len());
        }
        arr.remove(index);
        Ok(())
    })
}

/// Remove a nested TOML table (or key) at the given dot-separated key path.
///
/// For example, `remove_toml_table(path, "llm.backends.claude")` removes
/// the `[llm.backends.claude]` table and all its contents.
pub fn remove_toml_table(path: &Path, key: &str) -> anyhow::Result<()> {
    with_locked_doc(path, |doc| {
        let segments = split_key_path(key);
        if segments.is_empty() {
            anyhow::bail!("empty key path");
        }
        let (parents, leaf) = segments.split_at(segments.len() - 1);
        let mut table = doc.as_table_mut();
        for seg in parents {
            table = table.get_mut(seg)
                .and_then(|v| v.as_table_mut())
                .ok_or_else(|| anyhow::anyhow!("table '{}' not found in path '{}'", seg, key))?;
        }
        table.remove(&leaf[0]);
        Ok(())
    })
}

/// Replace the element at `index` in a TOML array at the given key path.
pub fn replace_in_toml_array(path: &Path, key: &str, index: usize, value: &str) -> anyhow::Result<()> {
    with_locked_doc(path, |doc| {
        let arr = get_or_create_array(doc, key)?;
        if index >= arr.len() {
            anyhow::bail!("index {} out of bounds (len={})", index, arr.len());
        }
        arr.replace(index, value);
        Ok(())
    })
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
            "daemon_addr = \"localhost:9500\"\n\n# First-run onboarding completed\n# onboarded = false\n",
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

        set_toml_value_nested(&path, "proxy.http_proxy", "http://proxy:8080").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("http_proxy = \"http://proxy:8080\""));
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
    fn test_set_nested_with_dotted_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "").unwrap();

        // Backend name "gemini-3.1" contains a dot — must be quoted in the key path
        set_toml_value_nested(&path, "llm.backends.\"gemini-3.1\".model", "gemini-3.1-pro").unwrap();
        set_toml_value_nested(&path, "llm.backends.\"gemini-3.1\".backend_type", "openai-compat").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        // toml_edit should produce a proper quoted key
        let parsed: toml_edit::DocumentMut = result.parse().unwrap();
        let backends = parsed["llm"]["backends"].as_table().unwrap();
        assert!(backends.contains_key("gemini-3.1"), "key 'gemini-3.1' not found in: {}", result);
        assert_eq!(backends["gemini-3.1"]["model"].as_str(), Some("gemini-3.1-pro"));
        assert_eq!(backends["gemini-3.1"]["backend_type"].as_str(), Some("openai-compat"));
    }

    #[test]
    fn test_split_key_path() {
        use super::split_key_path;
        assert_eq!(split_key_path("a.b.c"), vec!["a", "b", "c"]);
        assert_eq!(split_key_path("llm.backends.\"gemini-3.1\".model"),
            vec!["llm", "backends", "gemini-3.1", "model"]);
        assert_eq!(split_key_path("simple"), vec!["simple"]);
        assert_eq!(split_key_path("\"dotted.key\""), vec!["dotted.key"]);
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

    #[test]
    fn test_append_to_empty_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "").unwrap();

        append_to_toml_array(&path, "sandbox.plugins.bash.permit_rules", "command starts_with git").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = result.parse().unwrap();
        let arr = doc["sandbox"]["plugins"]["bash"]["permit_rules"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr.get(0).unwrap().as_str(), Some("command starts_with git"));
    }

    #[test]
    fn test_append_to_existing_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "[sandbox.plugins.bash]\npermit_rules = [\"command starts_with git\"]\n").unwrap();

        append_to_toml_array(&path, "sandbox.plugins.bash.permit_rules", "command contains glab").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = result.parse().unwrap();
        let arr = doc["sandbox"]["plugins"]["bash"]["permit_rules"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr.get(1).unwrap().as_str(), Some("command contains glab"));
    }

    #[test]
    fn test_remove_from_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "[sandbox.plugins.bash]\npermit_rules = [\"a\", \"b\", \"c\"]\n").unwrap();

        remove_from_toml_array(&path, "sandbox.plugins.bash.permit_rules", 1).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = result.parse().unwrap();
        let arr = doc["sandbox"]["plugins"]["bash"]["permit_rules"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr.get(0).unwrap().as_str(), Some("a"));
        assert_eq!(arr.get(1).unwrap().as_str(), Some("c"));
    }

    #[test]
    fn test_remove_out_of_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "[sandbox.plugins.bash]\npermit_rules = [\"a\"]\n").unwrap();

        let err = remove_from_toml_array(&path, "sandbox.plugins.bash.permit_rules", 5);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("out of bounds"));
    }

    #[test]
    fn test_replace_in_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "[sandbox.plugins.bash]\npermit_rules = [\"old_rule\", \"keep\"]\n").unwrap();

        replace_in_toml_array(&path, "sandbox.plugins.bash.permit_rules", 0, "new_rule").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = result.parse().unwrap();
        let arr = doc["sandbox"]["plugins"]["bash"]["permit_rules"].as_array().unwrap();
        assert_eq!(arr.get(0).unwrap().as_str(), Some("new_rule"));
        assert_eq!(arr.get(1).unwrap().as_str(), Some("keep"));
    }

    #[test]
    fn test_replace_out_of_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "[sandbox.plugins.bash]\npermit_rules = []\n").unwrap();

        let err = replace_in_toml_array(&path, "sandbox.plugins.bash.permit_rules", 0, "x");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("out of bounds"));
    }

    #[test]
    fn test_remove_toml_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "\
[llm]\ndefault = \"claude\"\n\n\
[llm.backends.claude]\nbackend_type = \"anthropic\"\nmodel = \"claude-sonnet\"\n\n\
[llm.backends.openai]\nbackend_type = \"openai-compat\"\nmodel = \"gpt-4o\"\n").unwrap();

        remove_toml_table(&path, "llm.backends.claude").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(!result.contains("[llm.backends.claude]"), "claude backend table should be removed");
        assert!(!result.contains("claude-sonnet"), "claude model should be removed");
        assert!(result.contains("[llm.backends.openai]"), "openai backend should remain");
        assert!(result.contains("gpt-4o"), "openai model should remain");
        assert!(result.contains("default"), "default setting should remain");
    }
}
