//! Client-side tool execution via short-lived plugin processes.
//! Spawns a fresh process per tool call: writes JSON to stdin, reads JSON from stdout.

use std::io::{BufRead, BufReader, Write};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Executes client-side tools by spawning short-lived plugin processes.
pub struct ClientPluginManager {
    plugin_bin: std::path::PathBuf,
}

#[derive(serde::Deserialize)]
struct PluginResponse {
    content: String,
    #[serde(default)]
    is_error: bool,
}

impl ClientPluginManager {
    pub fn new() -> Self {
        let plugin_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("omnish-plugin")))
            .unwrap_or_else(|| std::path::PathBuf::from("omnish-plugin"));
        Self { plugin_bin }
    }

    /// Execute a tool via a short-lived plugin process.
    ///
    /// - `plugin_name`: "builtin" or external plugin directory name
    /// - `tool_name`: the specific tool within the plugin
    /// - `input`: tool input JSON
    /// - `cwd`: optional working directory to inject into input
    /// - `sandboxed`: whether to apply Landlock sandbox
    pub fn execute_tool(
        &self,
        plugin_name: &str,
        tool_name: &str,
        input: &serde_json::Value,
        cwd: Option<&str>,
        sandboxed: bool,
    ) -> (String, bool) {
        let executable = if plugin_name == "builtin" {
            self.plugin_bin.clone()
        } else {
            omnish_common::config::omnish_dir()
                .join("plugins")
                .join(plugin_name)
                .join(plugin_name)
        };

        // Inject cwd into input if available
        let effective_input = if let Some(cwd) = cwd {
            let mut patched = input.clone();
            if let Some(obj) = patched.as_object_mut() {
                obj.insert("cwd".to_string(), serde_json::Value::String(cwd.to_string()));
            }
            patched
        } else {
            input.clone()
        };

        let request = serde_json::json!({
            "name": tool_name,
            "input": effective_input,
        });

        let data_dir = omnish_common::config::omnish_dir()
            .join("data")
            .join(plugin_name);
        let _ = std::fs::create_dir_all(&data_dir);

        let cwd_path: Option<std::path::PathBuf> = cwd.map(std::path::PathBuf::from);

        // On macOS: wrap with sandbox-exec; on Linux: use pre_exec Landlock
        #[cfg(target_os = "macos")]
        let mut cmd = if sandboxed {
            let mut c = Command::new("sandbox-exec");
            let profile = omnish_plugin::sandbox_profile(
                &data_dir,
                cwd_path.as_deref(),
            );
            c.args([
                "-p",
                &profile,
                &executable.to_string_lossy(),
            ]);
            c
        } else {
            Command::new(&executable)
        };

        #[cfg(not(target_os = "macos"))]
        let mut cmd = Command::new(&executable);

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        // Apply Landlock sandbox via pre_exec on Linux
        #[cfg(target_os = "linux")]
        if sandboxed {
            let data_dir_clone = data_dir.clone();
            let cwd_clone = cwd_path.clone();
            unsafe {
                cmd.pre_exec(move || {
                    omnish_plugin::apply_sandbox(&data_dir_clone, cwd_clone.as_deref()).map_err(
                        |e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e),
                    )
                });
            }
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return (format!("Failed to spawn plugin '{}': {}", plugin_name, e), true),
        };

        // Write request to stdin
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{}", serde_json::to_string(&request).unwrap());
            // stdin dropped here, closing it
        }

        // Read response from stdout
        let result = if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => ("Plugin produced no output".to_string(), true),
                Ok(_) => match serde_json::from_str::<PluginResponse>(&line) {
                    Ok(resp) => (resp.content, resp.is_error),
                    Err(e) => (format!("Invalid plugin response: {e}"), true),
                },
                Err(e) => (format!("Failed to read plugin output: {e}"), true),
            }
        } else {
            ("No stdout from plugin".to_string(), true)
        };

        let _ = child.wait();
        result
    }
}
