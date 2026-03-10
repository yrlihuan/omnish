use omnish_daemon::plugin::{Plugin, PluginType};
use omnish_daemon::tools::bash::BashTool;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    method: String,
    id: u64,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<serde_json::Value>,
}

fn resolve_plugin(name: &str) -> Option<Box<dyn Plugin>> {
    match name {
        "bash" => Some(Box::new(BashTool::new())),
        _ => None,
    }
}

fn run_plugin_mode(plugin: Box<dyn Plugin>) {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: 0,
                    result: None,
                    error: Some(serde_json::json!({"message": format!("parse error: {e}")})),
                };
                let _ = writeln!(writer, "{}", serde_json::to_string(&resp).unwrap());
                let _ = writer.flush();
                continue;
            }
        };

        let resp = match req.method.as_str() {
            "initialize" => {
                let plugin_type = match plugin.plugin_type() {
                    PluginType::ClientTool => "client_tool",
                    PluginType::DaemonTool => "daemon_tool",
                };
                let mut result = serde_json::json!({
                    "name": plugin.name(),
                    "tools": plugin.tools(),
                    "plugin_type": plugin_type,
                });
                if let Some(prompt) = plugin.system_prompt() {
                    result["system_prompt"] = serde_json::Value::String(prompt);
                }
                JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(result),
                    error: None,
                }
            }
            "tool/execute" => {
                let tool_name = req.params["name"].as_str().unwrap_or("");
                let input = &req.params["input"];
                let result = plugin.call_tool(tool_name, input);
                JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(serde_json::json!({
                        "content": result.content,
                        "is_error": result.is_error,
                    })),
                    error: None,
                }
            }
            "shutdown" => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(serde_json::json!({})),
                    error: None,
                };
                let _ = writeln!(writer, "{}", serde_json::to_string(&resp).unwrap());
                let _ = writer.flush();
                break;
            }
            other => JsonRpcResponse {
                jsonrpc: "2.0",
                id: req.id,
                result: None,
                error: Some(serde_json::json!({"message": format!("unknown method: {other}")})),
            },
        };

        let _ = writeln!(writer, "{}", serde_json::to_string(&resp).unwrap());
        let _ = writer.flush();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: omnish-plugin <plugin-name>");
        eprintln!("Available plugins: bash");
        std::process::exit(1);
    }

    let name = &args[1];

    if name == "--version" || name == "-V" {
        println!("omnish-plugin {}", omnish_common::VERSION);
        return;
    }

    let plugin = match resolve_plugin(name) {
        Some(p) => p,
        None => {
            eprintln!("Unknown plugin: {name}");
            eprintln!("Available plugins: bash");
            std::process::exit(1);
        }
    };

    run_plugin_mode(plugin);
}
