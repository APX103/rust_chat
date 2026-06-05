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
    path.push("target/debug/mcp-filesystem-rs");
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

    // 模拟 mini-agent mcp.rs 中 JsonRpcResponse 的严格校验
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
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"write_file"));
    assert!(names.contains(&"list_directory"));

    child.kill().ok();
}

#[test]
fn test_read_write_list_roundtrip() {
    let bin = build_server();
    let tmp = std::env::temp_dir().join(format!("mcp-fs-test-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // write_file
    let test_file = tmp.join("roundtrip.txt");
    send(
        &mut stdin,
        10,
        "tools/call",
        serde_json::json!({
            "name": "write_file",
            "arguments": {
                "path": test_file.to_str().unwrap(),
                "content": "hello rust"
            }
        }),
    );
    let resp = recv(&mut stdout);
    assert_eq!(resp["id"].as_u64(), Some(10));
    assert_eq!(resp["result"]["isError"].as_bool(), Some(false));

    // read_file
    send(
        &mut stdin,
        11,
        "tools/call",
        serde_json::json!({
            "name": "read_file",
            "arguments": {"path": test_file.to_str().unwrap()}
        }),
    );
    let resp = recv(&mut stdout);
    assert_eq!(resp["id"].as_u64(), Some(11));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, "hello rust");

    // list_directory
    send(
        &mut stdin,
        12,
        "tools/call",
        serde_json::json!({
            "name": "list_directory",
            "arguments": {"path": tmp.to_str().unwrap()}
        }),
    );
    let resp = recv(&mut stdout);
    assert_eq!(resp["id"].as_u64(), Some(12));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("roundtrip.txt"),
        "directory listing should contain roundtrip.txt: {}",
        text
    );

    child.kill().ok();
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_error_cases() {
    let bin = build_server();
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // missing file
    send(
        &mut stdin,
        20,
        "tools/call",
        serde_json::json!({
            "name": "read_file",
            "arguments": {"path": "/this/path/does/not/exist/for/sure.txt"}
        }),
    );
    let resp = recv(&mut stdout);
    assert_eq!(resp["id"].as_u64(), Some(20));
    assert_eq!(resp["result"]["isError"].as_bool(), Some(true));

    // unknown tool
    send(
        &mut stdin,
        21,
        "tools/call",
        serde_json::json!({
            "name": "unknown_tool",
            "arguments": {}
        }),
    );
    let resp = recv(&mut stdout);
    assert_eq!(resp["id"].as_u64(), Some(21));
    assert_eq!(resp["result"]["isError"].as_bool(), Some(true));

    child.kill().ok();
}
