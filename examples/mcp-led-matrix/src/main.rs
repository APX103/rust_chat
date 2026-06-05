//! Rust stdio MCP server — LED Matrix control for Arduino UNO Q
//! Talks to arduino-router via Unix Socket + MessagePack-RPC
//! Zero Node.js / Zero Python

use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use serde_json::{json, Value};

const SOCKET_PATH: &str = "/var/run/arduino-router.sock";

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
                    "serverInfo": {"name": "led-matrix-rs", "version": "1.0"}
                }));
            }
            "tools/list" => {
                respond(&mut stdout, &id, json!({
                    "tools": [
                        {
                            "name": "matrix_draw",
                            "description": "Draw a pattern on the 8x13 LED matrix. 104 values (0 or 1) in row-major order.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "pattern": {
                                        "type": "array",
                                        "items": {"type": "integer", "minimum": 0, "maximum": 1},
                                        "minItems": 104,
                                        "maxItems": 104,
                                        "description": "104 integers (0 or 1) in row-major order: 8 rows x 13 cols. Example heart: [0,0,0,0,1,0,0,0,0,0,0,0,0, 0,0,0,1,1,1,0,0,0,0,0,0,0, 0,0,1,1,1,1,1,0,0,0,0,0,0, 0,1,1,1,1,1,1,1,0,0,0,0,0, 0,1,1,1,1,1,1,1,0,0,0,0,0, 0,0,1,1,1,1,1,0,0,0,0,0,0, 0,0,0,1,1,1,0,0,0,0,0,0,0, 0,0,0,0,1,0,0,0,0,0,0,0,0]"
                                    }
                                },
                                "required": ["pattern"]
                            }
                        },
                        {
                            "name": "matrix_clear",
                            "description": "Clear the LED matrix (turn all LEDs off)",
                            "inputSchema": {
                                "type": "object",
                                "properties": {}
                            }
                        },
                        {
                            "name": "matrix_set_grayscale",
                            "description": "Set grayscale bit depth: 1=2 levels (on/off), 2=4 levels, 3=8 levels",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "bits": {"type": "integer", "minimum": 1, "maximum": 3}
                                },
                                "required": ["bits"]
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
                    "matrix_draw" => {
                        let pattern = args.get("pattern").and_then(|p| p.as_array()).cloned().unwrap_or_default();
                        let bits: String = pattern.iter()
                            .map(|v| if v.as_u64().unwrap_or(0) > 0 { '1' } else { '0' })
                            .collect();
                        match call_rpc("matrix_draw", vec![rmpv::Value::String(bits.into())]) {
                            Ok(_) => tool_result("Pattern drawn on LED matrix", false),
                            Err(e) => tool_result(&e, true),
                        }
                    }
                    "matrix_clear" => {
                        match call_rpc("matrix_clear", vec![]) {
                            Ok(_) => tool_result("LED matrix cleared", false),
                            Err(e) => tool_result(&e, true),
                        }
                    }
                    "matrix_set_grayscale" => {
                        let bits = args.get("bits").and_then(|b| b.as_u64()).unwrap_or(1) as u8;
                        match call_rpc("matrix_set_grayscale", vec![rmpv::Value::Integer(bits.into())]) {
                            Ok(_) => tool_result(&format!("Grayscale set to {} bits", bits), false),
                            Err(e) => tool_result(&e, true),
                        }
                    }
                    _ => tool_result("Unknown tool", true),
                };

                respond_raw(&mut stdout, &result);
            }
            _ => {}
        }
    }
}

fn call_rpc(method: &str, params: Vec<rmpv::Value>) -> Result<rmpv::Value, String> {
    let mut sock = UnixStream::connect(SOCKET_PATH)
        .map_err(|e| format!("Socket connect failed: {}", e))?;

    let request = rmpv::Value::Array(vec![
        rmpv::Value::Integer(0.into()),
        rmpv::Value::Integer(1.into()),
        rmpv::Value::String(method.into()),
        rmpv::Value::Array(params),
    ]);

    let packed = rmp_serde::to_vec(&request)
        .map_err(|e| format!("Pack failed: {}", e))?;
    sock.write_all(&packed)
        .map_err(|e| format!("Write failed: {}", e))?;

    let mut buf = [0u8; 1024];
    let n = sock.read(&mut buf)
        .map_err(|e| format!("Read failed: {}", e))?;

    let response: Vec<rmpv::Value> = rmp_serde::from_slice(&buf[..n])
        .map_err(|e| format!("Unpack failed: {}", e))?;

    if response.len() >= 4 {
        if let Some(err) = response[2].as_str() {
            if !err.is_empty() {
                return Err(format!("RPC error: {}", err));
            }
        }
        return Ok(response[3].clone());
    }

    Err("Invalid response format".to_string())
}

fn respond(out: &mut impl Write, id: &Value, result: Value) {
    let msg = json!({"jsonrpc": "2.0", "id": id, "result": result});
    writeln!(out, "{}", msg).ok();
    out.flush().ok();
}

fn tool_result(text: &str, is_error: bool) -> String {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error
    }).to_string()
}

fn respond_raw(out: &mut impl Write, msg: &str) {
    writeln!(out, "{}", msg).ok();
    out.flush().ok();
}
