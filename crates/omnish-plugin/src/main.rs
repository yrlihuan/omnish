use omnish_llm::tool::Tool;
use omnish_plugin::tools::bash::BashTool;
use omnish_plugin::tools::edit::EditTool;
use omnish_plugin::tools::read::ReadTool;
use omnish_plugin::tools::write::WriteTool;
use std::io::{BufRead, Write};

#[derive(serde::Deserialize)]
struct Request {
    name: String,
    input: serde_json::Value,
}

#[derive(serde::Serialize)]
struct Response {
    content: String,
    is_error: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && (args[1] == "--version" || args[1] == "-V") {
        println!("omnish-plugin {}", omnish_common::VERSION);
        return;
    }

    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!("omnish-plugin: no input on stdin");
        std::process::exit(1);
    }

    let req: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            let resp = Response {
                content: format!("Invalid input: {e}"),
                is_error: true,
            };
            println!("{}", serde_json::to_string(&resp).unwrap());
            return;
        }
    };

    let result = match req.name.as_str() {
        "bash" => BashTool::new().execute(&req.input),
        "read" => ReadTool::new().execute(&req.input),
        "edit" => EditTool::new().execute(&req.input),
        "write" => WriteTool::new().execute(&req.input),
        other => omnish_llm::tool::ToolResult {
            tool_use_id: String::new(),
            content: format!("Unknown tool: {other}"),
            is_error: true,
        },
    };

    let resp = Response {
        content: result.content,
        is_error: result.is_error,
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
}
