use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;

static BUILD_LOCK: Mutex<()> = Mutex::new(());

fn build_server() -> PathBuf {
    let _guard = BUILD_LOCK.lock().unwrap();
    let status = Command::new("cargo")
        .args(["build"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("failed to run cargo build");
    assert!(status.success(), "cargo build failed");

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/mcp-time-rs");
    path
}

fn send(stdin: &mut impl Write, id: u64, method: &str, params: Value) {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    writeln!(stdin, "{}", req).unwrap();
    stdin.flush().unwrap();
}

fn recv(stdout: &mut BufReader<impl std::io::Read>) -> Value {
    let mut line = String::new();
    let n = stdout.read_line(&mut line).unwrap();
    assert!(n > 0, "MCP server closed connection unexpectedly");
    let v: Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("failed to parse JSON-RPC response: {}\nraw: {}", e, line.trim()));

    assert_eq!(
        v.get("jsonrpc").and_then(|x| x.as_str()),
        Some("2.0"),
        "response must have jsonrpc=2.0 (this is what mini-agent requires)\nraw: {}",
        line.trim()
    );
    v
}

#[test]
fn test_initialize() {
    let bin = build_server();
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(
        &mut stdin,
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        }),
    );

    let resp = recv(&mut stdout);
    assert_eq!(resp["id"].as_u64(), Some(1));
    assert_eq!(resp["result"]["protocolVersion"].as_str(), Some("2024-11-05"));

    child.kill().ok();
}

#[test]
fn test_tools_list() {
    let bin = build_server();
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(&mut stdin, 1, "tools/list", Value::Null);
    let resp = recv(&mut stdout);
    assert_eq!(resp["id"].as_u64(), Some(1));
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"].as_str(), Some("get_current_time"));

    child.kill().ok();
}

#[test]
fn test_get_current_time() {
    let bin = build_server();
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(
        &mut stdin,
        42,
        "tools/call",
        serde_json::json!({
            "name": "get_current_time",
            "arguments": {}
        }),
    );
    let resp = recv(&mut stdout);
    assert_eq!(resp["id"].as_u64(), Some(42));
    assert_eq!(resp["result"]["isError"].as_bool(), Some(false));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("Current UNIX timestamp: "),
        "unexpected time response: {}",
        text
    );
    let ts: u64 = text
        .strip_prefix("Current UNIX timestamp: ")
        .unwrap()
        .parse()
        .expect("timestamp should be numeric");
    // 基本合理性校验：2020-01-01 到 2100-01-01 之间
    assert!(ts > 1577836800, "timestamp too old: {}", ts);
    assert!(ts < 4102444800, "timestamp too far in future: {}", ts);

    child.kill().ok();
}
