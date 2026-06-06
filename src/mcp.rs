//! MCP (Model Context Protocol) client for mini-agent.
//!
//! Supports stdio and HTTP/SSE transports.
//! Lightweight JSON-RPC implementation without external MCP SDK.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to build HTTP client")
    })
}

use crate::models::{ToolSchema, ToolSource};
use crate::tool_registry::ToolRegistry;

// ---------------------------------------------------------------------------
// MCP JSON-RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct ListToolsResult {
    tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Deserialize)]
struct CallToolResult {
    content: Vec<ToolContent>,
    #[serde(default)]
    is_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ToolContent {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default)]
    text: Option<String>,
}

// ---------------------------------------------------------------------------
// MCP Server connection
// ---------------------------------------------------------------------------

pub enum McpTransport {
    Stdio {
        child: Arc<Mutex<Child>>,
        stdin: Arc<Mutex<ChildStdin>>,
        stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    },
    Http {
        base_url: String,
        headers: HashMap<String, String>,
        streamable: bool,
        session_id: Option<String>,
    },
}

pub struct McpServer {
    name: String,
    transport: Arc<Mutex<McpTransport>>,
    request_id: AtomicU64,
    timeout_secs: u64,
}

impl McpServer {
    pub fn connect_stdio(
        name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        timeout: u64,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        
        // Filtered environment (security)
        let safe_keys: std::collections::HashSet<&str> = [
            "PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM", "SHELL", "TMPDIR",
        ].iter().cloned().collect();
        
        cmd.env_clear();
        for (key, val) in std::env::vars() {
            if safe_keys.contains(key.as_str()) || key.starts_with("XDG_") {
                cmd.env(key, val);
            }
        }
        for (key, val) in env {
            cmd.env(key, val);
        }
        
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        
        let mut child = cmd.spawn()
            .with_context(|| format!("Failed to spawn MCP server: {} {}", command, args.join(" ")))?;
        
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("Failed to get stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("Failed to get stdout"))?;
        
        let transport = McpTransport::Stdio {
            child: Arc::new(Mutex::new(child)),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
        };
        
        let server = Self {
            name: name.to_string(),
            transport: Arc::new(Mutex::new(transport)),
            request_id: AtomicU64::new(1),
            timeout_secs: timeout,
        };
        
        // Initialize session
        server.initialize()?;
        
        Ok(server)
    }
    
    pub fn connect_http(
        name: &str,
        url: &str,
        headers: &HashMap<String, String>,
        timeout: u64,
        streamable: bool,
    ) -> Result<Self> {
        let server = Self {
            name: name.to_string(),
            transport: Arc::new(Mutex::new(McpTransport::Http {
                base_url: url.to_string(),
                headers: headers.clone(),
                streamable,
                session_id: None,
            })),
            request_id: AtomicU64::new(1),
            timeout_secs: timeout,
        };
        // For HTTP/StreamableHttp MCP servers (e.g. Zhipu), initialize may not be required.
        // Try it, but don't fail if the server doesn't support it.
        if let Err(e) = server.initialize() {
            log::warn!("MCP server '{}' initialize failed (may not be required for HTTP MCP): {}", name, e);
        } else if streamable {
            // Streamable HTTP requires notifications/initialized after successful initialize
            if let Err(e) = server.send_initialized() {
                log::warn!("MCP server '{}' initialized notification failed: {}", name, e);
            }
        }
        Ok(server)
    }
    
    fn initialize(&self) -> Result<()> {
        let result = self.request("initialize", Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "mini-agent", "version": "0.1.0" }
        })))?;
        log::debug!("MCP server '{}' initialized: {:?}", self.name, result);
        Ok(())
    }

    /// Send notifications/initialized (required for Streamable HTTP).
    fn send_initialized(&self) -> Result<()> {
        // Notifications have no id and expect no response
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 0, // dummy id; we won't wait for response
            method: "notifications/initialized".to_string(),
            params: None,
        };
        let req_json = serde_json::to_string(&req)?;

        let transport = self.transport.lock().unwrap();
        if let McpTransport::Http { base_url, headers, streamable, session_id } = &*transport {
            let url = if *streamable {
                base_url.trim_end_matches('/').to_string()
            } else {
                format!("{}/message", base_url.trim_end_matches('/'))
            };
            let client = http_client();
            let mut request = client.post(&url)
                .timeout(Duration::from_secs(self.timeout_secs))
                .body(req_json)
                .header("Content-Type", "application/json");

            if *streamable {
                request = request.header("Accept", "application/json, text/event-stream");
            }
            if let Some(sid) = session_id {
                request = request.header("Mcp-Session-Id", sid.as_str());
            }
            for (k, v) in headers.iter() {
                request = request.header(k, v.as_str());
            }

            // Fire and forget — don't check response for notifications
            let _ = request.send();
            log::debug!("MCP server '{}' sent notifications/initialized", self.name);
        }
        Ok(())
    }
    
    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }
    
    fn request(&self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value> {
        let id = self.next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };
        let req_json = serde_json::to_string(&req)?;
        
        let mut transport = self.transport.lock().unwrap();
        match &mut *transport {
            McpTransport::Stdio { stdin, stdout, .. } => {
                // Write request
                {
                    let mut writer = stdin.lock().unwrap();
                    writeln!(writer, "{}", req_json)?;
                    writer.flush()?;
                }
                
                // Read response
                let mut reader = stdout.lock().unwrap();
                let mut line = String::new();
                let deadline = std::time::Instant::now() + Duration::from_secs(self.timeout_secs);
                
                while std::time::Instant::now() < deadline {
                    match reader.read_line(&mut line) {
                        Ok(0) => return Err(anyhow!("MCP server closed connection")),
                        Ok(_) => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                let resp: JsonRpcResponse = serde_json::from_str(trimmed)
                                    .with_context(|| format!("Invalid JSON-RPC response: {}", trimmed))?;
                                if resp.id == Some(id) {
                                    if let Some(err) = resp.error {
                                        return Err(anyhow!("MCP error {}: {}", err.code, err.message));
                                    }
                                    return Ok(resp.result.unwrap_or(serde_json::Value::Null));
                                }
                            }
                            line.clear();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(anyhow!("MCP request timeout"))
            }
            McpTransport::Http { base_url, headers, streamable, session_id } => {
                let url = if *streamable {
                    base_url.trim_end_matches('/').to_string()
                } else {
                    format!("{}/message", base_url.trim_end_matches('/'))
                };
                let client = http_client();
                let mut request = client.post(&url)
                    .timeout(Duration::from_secs(self.timeout_secs))
                    .json(&req);

                if *streamable {
                    request = request.header("Accept", "application/json, text/event-stream");
                }
                if let Some(sid) = session_id {
                    request = request.header("Mcp-Session-Id", sid.as_str());
                }
                for (k, v) in headers.iter() {
                    request = request.header(k, v.as_str());
                }

                let resp = request.send()
                    .with_context(|| format!("MCP HTTP request failed: {}", url))?;

                // Capture session id from response headers before consuming body
                let new_session_id = resp.headers().get("Mcp-Session-Id")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                let status = resp.status();
                if !status.is_success() {
                    let err_text = resp.text().unwrap_or_default();
                    return Err(anyhow!("MCP HTTP {}: {}", status, err_text));
                }

                let resp_text = resp.text()
                    .with_context(|| "Failed to read HTTP response body")?;

                // Update session id if server returned one
                if let Some(sid) = new_session_id {
                    *session_id = Some(sid);
                }

                // Parse response: may be pure JSON or SSE format
                let json_text = if resp_text.contains("event:") {
                    // SSE format: extract data lines
                    parse_sse_json(&resp_text)?
                } else {
                    resp_text
                };

                let resp_json: JsonRpcResponse = serde_json::from_str(&json_text)
                    .with_context(|| format!("Failed to parse JSON-RPC response: {}", &json_text[..json_text.len().min(200)]))?;

                if let Some(err) = resp_json.error {
                    return Err(anyhow!("MCP error {}: {}", err.code, err.message));
                }
                Ok(resp_json.result.unwrap_or(serde_json::Value::Null))
            }
        }
    }
    
    pub fn list_tools(&self) -> Result<Vec<McpTool>> {
        let result = self.request("tools/list", None)?;
        let tools_result: ListToolsResult = serde_json::from_value(result)
            .unwrap_or(ListToolsResult { tools: vec![] });
        Ok(tools_result.tools)
    }
    
    pub fn call_tool(&self, tool_name: &str, arguments: serde_json::Value) -> Result<String> {
        let result = self.request("tools/call", Some(json!({
            "name": tool_name,
            "arguments": arguments
        })))?;
        
        let call_result: CallToolResult = serde_json::from_value(result)
            .map_err(|e| anyhow!("Failed to parse tool result: {}", e))?;
        
        let mut texts = vec![];
        for content in call_result.content {
            if content.content_type == "text" {
                if let Some(text) = content.text {
                    texts.push(text);
                }
            }
        }
        
        let output = if texts.is_empty() {
            "(empty result)".to_string()
        } else {
            texts.join("\n")
        };
        
        if call_result.is_error {
            Err(anyhow!("Tool error: {}", output))
        } else {
            Ok(output)
        }
    }
    
    pub fn shutdown(&self) -> Result<()> {
        self.request("shutdown", None).ok();
        
        let transport = self.transport.lock().unwrap();
        if let McpTransport::Stdio { child, .. } = &*transport {
            if let Ok(mut c) = child.lock() {
                c.kill().ok();
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SSE response parser for StreamableHttp MCP servers
// ---------------------------------------------------------------------------

fn parse_sse_json(text: &str) -> Result<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("data:") {
            return Ok(trimmed[5..].trim().to_string());
        }
    }
    Err(anyhow!("No data line found in SSE response"))
}

// ---------------------------------------------------------------------------
// MCP Client Manager
// ---------------------------------------------------------------------------

pub struct McpClientManager {
    servers: Arc<Mutex<HashMap<String, Arc<McpServer>>>>,
}

impl McpClientManager {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    pub fn connect_servers(
        &self,
        configs: &HashMap<String, crate::models::McpServerConfig>,
    ) -> Result<Vec<String>> {
        let mut tool_names = vec![];
        
        for (name, config) in configs {
            if self.servers.lock().unwrap().contains_key(name) {
                continue;
            }
            
            log::info!("Connecting to MCP server: {}", name);
            
            let server_result = if let Some(cmd) = &config.command {
                McpServer::connect_stdio(
                    name,
                    cmd,
                    config.args.as_deref().unwrap_or(&[]),
                    &config.env,
                    config.timeout,
                )
            } else if let Some(url) = &config.url {
                // Prefer explicit headers; fall back to env keys that look like HTTP headers
                let mut headers = config.headers.clone();
                if headers.is_empty() {
                    for (k, v) in &config.env {
                        let kl = k.to_lowercase();
                        if kl.starts_with("authorization") || kl.starts_with("x-") {
                            headers.insert(k.clone(), v.clone());
                        }
                    }
                }
                McpServer::connect_http(name, url, &headers, config.timeout, config.is_streamable_http())
            } else {
                log::warn!("MCP server '{}' has no command or url", name);
                continue;
            };
            
            let server = match server_result {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("MCP server '{}' connection failed: {}", name, e);
                    continue;
                }
            };
            
            let server = Arc::new(server);
            self.servers.lock().unwrap().insert(name.clone(), server.clone());
            
            // Discover tools
            match server.list_tools() {
                Ok(tools) => {
                    for tool in tools {
                        let prefixed_name = format!("mcp_{}_{}", sanitize_name(name), sanitize_name(&tool.name));
                        log::info!("  Registered MCP tool: {}", prefixed_name);
                        tool_names.push(prefixed_name.clone());
                        
                        // Store in registry will happen outside
                    }
                }
                Err(e) => {
                    log::warn!("Failed to list tools for MCP server '{}': {}", name, e);
                }
            }
        }
        
        Ok(tool_names)
    }
    
    pub fn get_server(&self, name: &str) -> Option<Arc<McpServer>> {
        self.servers.lock().unwrap().get(name).cloned()
    }
    
    pub fn shutdown_all(&self) {
        let servers = self.servers.lock().unwrap();
        for (name, server) in servers.iter() {
            log::info!("Shutting down MCP server: {}", name);
            server.shutdown().ok();
        }
    }
}

impl Default for McpClientManager {
    fn default() -> Self {
        Self::new()
    }
}

fn sanitize_name(name: &str) -> String {
    name.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '_', "_")
        .replace("..", "_")
}

/// Register MCP tools into the global ToolRegistry
pub fn register_mcp_tools(
    registry: &ToolRegistry,
    mcp_manager: &McpClientManager,
    configs: &HashMap<String, crate::models::McpServerConfig>,
) -> Result<Vec<String>> {
    let mut all_tools = vec![];
    
    for (server_name, _config) in configs {
        let server = match mcp_manager.get_server(server_name) {
            Some(s) => s,
            None => {
                log::debug!("MCP server '{}' not connected, skipping tool registration", server_name);
                continue;
            }
        };
        
        let tools = match server.list_tools() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Failed to list tools for MCP server '{}': {}", server_name, e);
                continue;
            }
        };
        for tool in tools {
            let prefixed_name = format!("mcp_{}_{}", sanitize_name(server_name), sanitize_name(&tool.name));
            let schema = ToolSchema {
                name: prefixed_name.clone(),
                description: format!("[MCP:{}] {}", server_name, tool.description),
                parameters: normalize_schema(tool.input_schema),
            };
            
            let server_clone = server.clone();
            let tool_name = tool.name.clone();
            let server_name_clone = server_name.clone();
            let handler = move |_name: &str, args: &serde_json::Value| -> anyhow::Result<String> {
                log::debug!("MCP tool call: {}::{} args={}", server_name_clone, tool_name, args);
                match server_clone.call_tool(&tool_name, args.clone()) {
                    Ok(result) => {
                        log::debug!("MCP tool result: {}::{} len={} result={}", server_name_clone, tool_name, result.len(), &result[..result.len().min(500)]);
                        Ok(result)
                    }
                    Err(e) => {
                        log::warn!("MCP tool error: {}::{} error={}", server_name_clone, tool_name, e);
                        Ok(format!("{{\"error\": \"{}\"}}", e))
                    }
                }
            };
            
            registry.register_tool_legacy(
                schema,
                Arc::new(handler),
                ToolSource::Mcp { server: server_name.clone() },
            );
            all_tools.push(prefixed_name);
        }
    }
    
    Ok(all_tools)
}

/// Normalize MCP input schema to OpenAI-compatible format
fn normalize_schema(schema: serde_json::Value) -> serde_json::Value {
    let mut schema = schema;
    if let Some(obj) = schema.as_object_mut() {
        if !obj.contains_key("type") {
            obj.insert("type".to_string(), json!("object"));
        }
        if !obj.contains_key("properties") {
            obj.insert("properties".to_string(), json!({}));
        }
        // Replace $defs with definitions for broader compatibility
        if let Some(defs) = obj.remove("$defs") {
            obj.insert("definitions".to_string(), defs);
        }
    }
    schema
}
