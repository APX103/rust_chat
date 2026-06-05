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

        // 极简解析：只关心 tool name
        if line.contains("initialize") {
            respond(&mut stdout, r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"time-rs","version":"1.0"}}}"#);
        } else if line.contains("tools/list") {
            respond(&mut stdout, r#"{"jsonrpc":"2.0","id":0,"result":{"tools":[{"name":"get_current_time","description":"Return current system time","inputSchema":{"type":"object","properties":{}}}]}"#);
        } else if line.contains("tools/call") {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let reply = format!(
                r#"{{"jsonrpc":"2.0","id":0,"result":{{"content":[{{"type":"text","text":"Current UNIX timestamp: {}"}}],"isError":false}}}}"#,
                now
            );
            respond(&mut stdout, &reply);
        }
    }
}

fn respond(out: &mut impl Write, msg: &str) {
    writeln!(out, "{}", msg).ok();
    out.flush().ok();
}
