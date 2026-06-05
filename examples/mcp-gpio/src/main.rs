//! Rust stdio MCP server — GPIO control for Arduino UNO Q
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
                    "serverInfo": {"name": "gpio-rs", "version": "1.0"}
                }));
            }
            "tools/list" => {
                respond(&mut stdout, &id, json!({
                    "tools": [
                        {
                            "name": "gpio_write",
                            "description": "Write HIGH or LOW to an Arduino GPIO pin (UNO Q)",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "pin": {"type": "integer", "description": "Arduino pin number, e.g. 13 for D13/LED"},
                                    "value": {"type": "boolean", "description": "true = HIGH, false = LOW"}
                                },
                                "required": ["pin", "value"]
                            }
                        },
                        {
                            "name": "gpio_read",
                            "description": "Read the digital value of an Arduino GPIO pin (UNO Q)",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "pin": {"type": "integer", "description": "Arduino pin number"}
                                },
                                "required": ["pin"]
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
                    "gpio_write" => {
                        let pin = args.get("pin").and_then(|p| p.as_u64()).unwrap_or(0) as u8;
                        let value = args.get("value").and_then(|v| v.as_bool()).unwrap_or(false);
                        match call_rpc("gpio_write", vec![
                            rmpv::Value::Integer(pin.into()),
                            rmpv::Value::Boolean(value),
                        ]) {
                            Ok(_) => tool_result(
                                &format!("Pin {} set to {}", pin, if value { "HIGH" } else { "LOW" }),
                                false,
                            ),
                            Err(e) => tool_result(&e, true),
                        }
                    }
                    "gpio_read" => {
                        let pin = args.get("pin").and_then(|p| p.as_u64()).unwrap_or(0) as u8;
                        match call_rpc("gpio_read", vec![rmpv::Value::Integer(pin.into())]) {
                            Ok(result) => {
                                let val = result.as_bool().unwrap_or(false);
                                tool_result(
                                    &format!("Pin {} = {}", pin, if val { "HIGH" } else { "LOW" }),
                                    false,
                                )
                            }
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

/// Send MessagePack-RPC request to arduino-router via Unix Socket
fn call_rpc(method: &str, params: Vec<rmpv::Value>) -> Result<rmpv::Value, String> {
    let mut sock = UnixStream::connect(SOCKET_PATH)
        .map_err(|e| format!("Socket connect failed: {}", e))?;

    // MessagePack-RPC request format: [type, msgid, method, params]
    let request = rmpv::Value::Array(vec![
        rmpv::Value::Integer(0.into()),      // type = request
        rmpv::Value::Integer(1.into()),      // msgid
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

    // Response format: [1, msgid, error, result]
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
