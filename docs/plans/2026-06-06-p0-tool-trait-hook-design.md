# P0: Tool Trait + Hook System 设计文档

> 日期：2026-06-06  
> 来源：脑暴 ZeroClaw 可借鉴特性  
> 状态：已批准

## 1. 背景

当前 `rust_chat` 的工具系统基于闭包注册，Hook 能力缺失。  
参考 ZeroClaw 的 `Tool` trait 和 `HookRunner` 系统，引入结构化工具接口和生命周期钩子。

## 2. Tool Trait

### 2.1 设计目标

- 类型安全的工具接口（编译期检查）
- 结构化返回（区分工具执行失败 vs 业务逻辑结果）
- 向后兼容（现有闭包注册零改动）
- 测试友好（mock 工具只需实现一个 trait）

### 2.2 新文件：`src/tool.rs`

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolOutput>;
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub success: bool,
    pub text: String,
    pub error: Option<String>,
}
```

### 2.3 闭包适配器

```rust
struct ClosureTool {
    name: String,
    description: String,
    parameters_schema: serde_json::Value,
    handler: Arc<dyn Fn(&str, &serde_json::Value) -> anyhow::Result<String> + Send + Sync>,
}

#[async_trait]
impl Tool for ClosureTool {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn parameters_schema(&self) -> serde_json::Value { self.parameters_schema.clone() }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolOutput> {
        let result = (self.handler)(self.name.as_str(), &args)?;
        Ok(ToolOutput { success: true, text: result, error: None })
    }
}
```

### 2.4 ToolRegistry 变更

| 变更 | 说明 |
|------|------|
| 新增 `register_tool(Arc<dyn Tool>)` | 接受 trait 对象 |
| 保留 `register_closure(schema, handler, source)` | 内部包装为 `ClosureTool` |
| `dispatch` 返回 `Result<ToolOutput>` | Agent 层根据 `success` 格式化 |

### 2.5 Agent 层变更

```rust
// 旧：
let result = registry.dispatch(name, args)?;
messages.push(Message::tool(id, name, result));

// 新：
let output = registry.dispatch(name, args).await?;
let content = if output.success {
    output.text
} else {
    format!("Error: {}", output.error.unwrap_or(output.text))
};
messages.push(Message::tool(id, name, content));
```

### 2.6 受影响文件

| 文件 | 改动 |
|------|------|
| `src/tool.rs` | 新建 — Tool trait + ToolOutput + ClosureTool |
| `src/tool_registry.rs` | 新增 `register_tool`，`dispatch` 返回 `ToolOutput` |
| `src/agent.rs` | 适配 `ToolOutput.success` 分支 |
| `src/mcp.rs` | 无改动（闭包注册走 `register_closure`） |
| `src/main.rs` | 无改动 |
| `src/models.rs` | 无改动 |

## 3. Hook 系统

### 3.1 设计目标

- 在 Agent 循环的关键节点注入自定义逻辑
- 两种钩子：Void（并行 fire-and-forget）和 Modifying（串行可修改/取消）
- 与现有 Observer 并行共存（Observer 只读观测，Hook 可读写）
- Hook panic 隔离（catch_unwind）

### 3.2 新文件：`src/hooks.rs`

```rust
pub enum HookResult<T> {
    Continue(T),
    Cancel(String),
}

#[async_trait]
pub trait HookHandler: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32 { 0 }

    // Void hooks（并行）
    async fn on_session_start(&self, _session_id: &str) {}
    async fn on_session_end(&self, _session_id: &str) {}
    async fn on_turn_start(&self, _turn: usize, _message: &str) {}
    async fn on_turn_end(&self, _turn: usize, _response: &str) {}
    async fn on_tool_call(&self, _name: &str, _args: &Value) {}
    async fn on_tool_result(&self, _name: &str, _success: bool, _duration: Duration) {}

    // Modifying hooks（串行，可 Cancel）
    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        HookResult::Continue((name, args))
    }
    async fn after_tool_result(&self, name: String, output: ToolOutput) -> HookResult<ToolOutput> {
        HookResult::Continue((name, output))
    }
}
```

### 3.3 HookRunner

```rust
pub struct HookRunner {
    handlers: Vec<Box<dyn HookHandler>>,
}

impl HookRunner {
    pub fn new() -> Self { Self { handlers: Vec::new() } }
    pub fn register(&mut self, handler: Box<dyn HookHandler>) { ... }

    // Void: join_all 并行
    pub async fn fire_session_start(&self, session_id: &str) { ... }
    pub async fn fire_tool_call(&self, name: &str, args: &Value) { ... }

    // Modifying: 串行 by priority，catch_unwind
    pub async fn run_before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> { ... }
}
```

### 3.4 Agent 集成点

```
Agent::run_conversation()
  ├── fire_session_start(session_id)
  ├── loop (iterations)
  │   ├── fire_turn_start(turn, message)
  │   ├── run_before_tool_call(name, args) → 可能 Cancel/修改
  │   ├── execute_tool_call → ToolOutput
  │   ├── run_after_tool_result(name, output) → 可能修改
  │   ├── fire_tool_result(name, success, duration)
  │   └── ...
  ├── fire_turn_end(turn, response)
  └── fire_session_end(session_id)
```

### 3.5 与 Observer 的关系

| | Observer | Hook |
|---|---|---|
| 方向 | 只读 | 读写 |
| 返回值 | 无 | 可 Cancel / 修改数据 |
| 执行方式 | 同步 | 异步 |
| 定位 | 日志/监控 | 扩展/拦截 |

两者在 Agent 中独立存在，互不干扰。

### 3.6 受影响文件

| 文件 | 改动 |
|------|------|
| `src/hooks.rs` | 新建 — HookResult + HookHandler + HookRunner |
| `src/agent.rs` | 集成 HookRunner，在循环节点 fire hooks |
| `src/main.rs` | 注册示例 hook（如审计日志） |
| `src/observer.rs` | 无改动 |
| `src/tool.rs` | 无改动（Hook 引用 ToolOutput） |

## 4. 实现顺序

1. `src/tool.rs` — Tool trait + ToolOutput + ClosureTool
2. `src/tool_registry.rs` — register_tool + dispatch 返回 ToolOutput
3. `src/agent.rs` — 适配 ToolOutput，集成 HookRunner
4. `src/hooks.rs` — HookHandler + HookRunner
5. `src/main.rs` — 注册示例 hook，验证编译

## 5. 不在此 scope

- SecurityPolicy / Sandbox（P1）
- RuntimeAdapter（P2）
- AIEOS 身份（P2）
- Delegation 工具（P3）
- Open Skills 生态（P3）
