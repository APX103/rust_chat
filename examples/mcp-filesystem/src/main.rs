//! Rust stdio MCP server — filesystem tools
//! Zero Node.js / Zero Python

use std::fs;
use std::io::{self, BufRead, Write};
use serde_json::{json, Value};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                respond(&mut stdout, &id, json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": {"name": "filesystem-rs", "version": "1.0"}
                }));
            }
            "tools/list" => {
                respond(&mut stdout, &id, json!({
                    "tools": [
                        {
                            "name": "read_file",
                            "description": "Read contents of a file",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "path": {"type": "string", "description": "Absolute file path"}
                                },
                                "required": ["path"]
                            }
                        },
                        {
                            "name": "write_file",
                            "description": "Write content to a file",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "path": {"type": "string"},
                                    "content": {"type": "string"}
                                },
                                "required": ["path", "content"]
                            }
                        },
                        {
                            "name": "list_directory",
                            "description": "List files and directories",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "path": {"type": "string"}
                                },
                                "required": ["path"]
                            }
                        }
                    ]
                }));
            }
            "tools/call" => {
                let params = req.get("params").and_then(|p| p.as_object()).cloned().unwrap_or_default();
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").and_then(|a| a.as_object()).cloned().unwrap_or_default();

                let result = match name {
                    "read_file" => {
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        match fs::read_to_string(path) {
                            Ok(content) => tool_result(&content, false),
                            Err(e) => tool_result(&format!("Error: {}", e), true),
                        }
                    }
                    "write_file" => {
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        match fs::write(path, content) {
                            Ok(_) => tool_result("OK", false),
                            Err(e) => tool_result(&format!("Error: {}", e), true),
                        }
                    }
                    "list_directory" => {
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        match fs::read_dir(path) {
                            Ok(entries) => {
                                let mut lines = Vec::new();
                                for entry in entries.flatten() {
                                    let name = entry.file_name().to_string_lossy().to_string();
                                    let typ = match entry.file_type() {
                                        Ok(t) if t.is_dir() => "[dir]",
                                        Ok(t) if t.is_file() => "[file]",
                                        _ => "[?]",
                                    };
                                    lines.push(format!("{} {}", typ, name));
                                }
                                tool_result(&lines.join("\n"), false)
                            }
                            Err(e) => tool_result(&format!("Error: {}", e), true),
                        }
                    }
                    _ => tool_result("Unknown tool", true),
                };

                respond(&mut stdout, &id, result);
            }
            _ => {}
        }
    }
}

fn respond(out: &mut impl Write, id: &Value, result: Value) {
    let msg = json!({"jsonrpc": "2.0", "id": id, "result": result});
    writeln!(out, "{}", msg).ok();
    out.flush().ok();
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error
    })
}
