//! 极简 Rust stdio MCP server —— 返回当前时间
//! 零外部依赖，纯标准库实现 JSON-RPC over stdio

use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let method = extract_string_field(trimmed, "method").unwrap_or_default();
        let id = extract_id_field(trimmed);

        match method.as_str() {
            "initialize" => {
                respond(&mut stdout, id, r#"{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"time-rs","version":"1.0"}}"#);
            }
            "tools/list" => {
                respond(&mut stdout, id, r#"{"tools":[{"name":"get_current_time","description":"Return current system time","inputSchema":{"type":"object","properties":{}}}]}"#);
            }
            "tools/call" => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let result = format!(
                    r#"{{"content":[{{"type":"text","text":"Current UNIX timestamp: {}"}}],"isError":false}}"#,
                    now
                );
                respond(&mut stdout, id, &result);
            }
            _ => {}
        }
    }
}

/// 发送 JSON-RPC 2.0 响应，id 与请求保持一致
fn respond(out: &mut impl Write, id: u64, result_json: &str) {
    let msg = format!(r#"{{"jsonrpc":"2.0","id":{},"result":{}}}"#, id, result_json);
    writeln!(out, "{}", msg).ok();
    out.flush().ok();
}

/// 从 JSON 字符串中提取字符串字段（简单实现，只处理无转义的情况）
fn extract_string_field(json: &str, key: &str) -> Option<String> {
    let pattern = format!(r#""{}""#, key);
    let start = json.find(&pattern)? + pattern.len();
    let rest = &json[start..];
    // 跳过空白和冒号
    let mut chars = rest.chars();
    for c in chars.by_ref() {
        if c == ':' {
            break;
        }
    }
    let rest: String = chars.collect();
    let rest = rest.trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let rest = &rest[1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 从 JSON 字符串中提取 id 字段（数字）
fn extract_id_field(json: &str) -> u64 {
    let pattern = r#""id""#;
    if let Some(pos) = json.find(pattern) {
        let rest = &json[pos + pattern.len()..];
        let mut chars = rest.chars();
        for c in chars.by_ref() {
            if c == ':' {
                break;
            }
        }
        let rest: String = chars.collect();
        let rest = rest.trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().unwrap_or(0)
    } else {
        0
    }
}
