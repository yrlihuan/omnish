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
