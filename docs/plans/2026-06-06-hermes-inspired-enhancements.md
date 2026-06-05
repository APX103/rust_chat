# 从 Hermes Agent 借鉴功能：mini-agent 增强设计

> 日期：2026-06-06
> 目标：在保持 mini-agent 简洁架构的前提下，借鉴 Hermes Agent 的 ReAct 鲁棒性、记忆系统和推理展示能力。

## 设计总览

mini-agent 已具备 Hermes 的核心骨架（ReAct 循环、5 层记忆、MCP 客户端、Skill 系统），但缺少三方面的精细化能力：

1. **Working Memory** — 当前会话中的高优先级上下文会被 50 条硬截断丢失
2. **Reasoning 提取** — 只支持一层 `reasoning_content`，无法展示 DeepSeek/Kimi/Qwen 的思考链
3. **工具防护栏** — 工具名写错、JSON 截断、连续失败均会导致浪费迭代预算

这三个改进独立可插拔，每个对应一个新文件，agent.rs 仅增加 ~30 行调用代码。零新依赖。

---

## Phase 1：Working Memory 层

### 问题

`MemoryManager::prefetch_all()` 目前只从 SQLite 召回历史记忆。但当前会话中刚发生的事情（如"刚才那个 503 错误的修复方案"）如果被 `conversation_history` 的 50 条上限裁剪，就永久丢失。Semantic Memory 的关键词匹配对上下文强相关但文本不重叠的信息召回率低。

### 方案

在 `MemoryManager` 中新增 `WorkingMemoryProvider`，用环形缓冲区存最近 N 轮的高优先级信息。

### 架构

```
MemoryManager
├── BuiltinMemoryProvider   → SQLite (semantic + episodic + profile)
├── WorkingMemoryProvider   → in-memory ring buffer (NEW)
└── [未来: 外部 MemoryProvider]
```

### WorkingMemoryProvider 设计

```rust
pub struct WorkingMemoryProvider {
    turns: RingBuffer<Turn>,          // 最近 N 轮的 (user, assistant) 对
    facts: Vec<MemoryFact>,           // 被标记为"重要"的事实
    max_turns: usize,                 // 默认 10
    max_facts: usize,                 // 默认 20
}

struct Turn {
    turn_number: usize,
    user: String,
    assistant: String,
    timestamp: chrono::DateTime<chrono::Utc>,
    tool_calls_used: Vec<String>,
}

struct MemoryFact {
    key: String,
    value: String,
    source: FactSource,  // UserExplicit / AgentInferred
    created_at: chrono::DateTime<chrono::Utc>,
}
```

### Ring Buffer 实现

使用固定大小的 `VecDeque`，push 时自动淘汰最老的 turn：

```rust
use std::collections::VecDeque;

struct RingBuffer<T> {
    data: VecDeque<T>,
    capacity: usize,
}

impl<T> RingBuffer<T> {
    fn push(&mut self, item: T) {
        if self.data.len() >= self.capacity {
            self.data.pop_front();
        }
        self.data.push_back(item);
    }
}
```

### prefetch 联合召回

`MemoryManager::prefetch_all()` 已遍历所有 provider。WorkingMemoryProvider 的 `prefetch()` 输出格式：

```xml
## Recent Conversation (Working Memory)
[turn 12] User: 那个 503 错误怎么修？
[turn 12] Assistant: 检查了 gateway 日志，发现是 upstream timeout...
[turn 13] User: 加个 retry 逻辑吧

## Important Facts
- project_target: Arduino UNO Q (Cortex-A57, Debian 7)
- binary_constraint: fully static musl-linked
```

和 SQLite 的 `## Relevant Memories` 自然区分。

### Facts 写入方式

agent 通过内置 `memory` 工具的新 action 写入：

```rust
// memory tool 新增 action: "remember_working"
// agent.rs 中 sync_all 之后，检查是否有 working memory facts 要写入
memory(action="remember_working", key="project_target", value="Arduino UNO Q")
```

或者自动提取：当用户说"记住..."、"important:"等模式时，自动写入 facts。

### 设计决策

| 决策 | 选择 | 原因 |
|------|------|------|
| Ring buffer vs Vec | VecDeque 固定容量 | 自动淘汰，不无限增长 |
| Facts 是否可淘汰 | 独立上限，不被 ring buffer 淘汰 | 用户显式要求记住的应更持久 |
| prefetch 时机 | 不变，每轮 API 调用前 | 不增加 API 调用次数 |
| 会话隔离 | per-session，新 session 清空 | 不同会话的上下文不混淆 |
| sync_turn 开销 | 只追加 Turn，零 SQLite 写入 | 不影响现有性能 |

### 改动文件

| 文件 | 改动 |
|------|------|
| `src/memory.rs` | 加 `RingBuffer`、`Turn`、`MemoryFact`、`WorkingMemoryProvider`、`FactSource` |
| `src/agent.rs` | 无需改动（MemoryManager 已遍历所有 provider） |

---

## Phase 2：Reasoning 提取与展示

### 问题

当前 `agent.rs:295` 只提取了一层 `reasoning_content`：

```rust
if let Some(reasoning) = &assistant_msg.reasoning {
    log::debug!("Reasoning: {}...", &reasoning[..reasoning.len().min(200)]);
}
```

不同模型的推理内容字段不同：

| 层级 | 字段 | 来源模型 | 格式 |
|------|------|----------|------|
| 1 | `message.reasoning` | DeepSeek, Qwen | 直接字符串 |
| 2 | `message.reasoning_content` | Moonshot, Novita | 直接字符串 |
| 3 | `message.reasoning_details` | OpenRouter 统一 | `[{type, summary}, ...]` |
| 4 | XML 内联标签 | MiniMax-M2.7 等 | `<think>...</think>` 嵌入 content |

流式场景更复杂：`<think>` 标签可能跨 chunk 边界，需要状态机。

### 方案

在 `llm.rs` 内部实现 `extract_reasoning()`，`agent.rs` 通过 `on_reasoning` 回调感知。

### llm.rs 新增

```rust
/// 从 LLM 响应中提取推理内容（4 层优先级链）
pub fn extract_reasoning(msg: &ResponseMessage) -> Option<String> {
    // 1. reasoning 字段
    if let Some(r) = &msg.reasoning {
        if !r.trim().is_empty() { return Some(r.clone()); }
    }
    // 2. reasoning_content 字段
    if let Some(r) = &msg.reasoning_content {
        if !r.trim().is_empty() { return Some(r.clone()); }
    }
    // 3. reasoning_details 数组
    if let Some(details) = &msg.reasoning_details {
        let summaries: Vec<String> = details.iter()
            .filter_map(|d| d.summary.as_ref())
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect();
        if !summaries.is_empty() { return Some(summaries.join("\n")); }
    }
    // 4. 内联 XML 标签
    extract_inline_xml_reasoning(msg.content.as_deref().unwrap_or(""))
}

/// 从 content 中提取 <think>...</think> 等标签
fn extract_inline_xml_reasoning(content: &str) -> Option<String> {
    let patterns = [
        r"(?s)<think>(.*?)</think>",
        r"(?s)<thinking>(.*?)</thinking>",
        r"(?s)<thought>(.*?)</thought>",
        r"(?s)<reasoning>(.*?)</reasoning>",
        r"(?s)<REASONING_SCRATCHPAD>(.*?)</REASONING_SCRATCHPAD>",
    ];
    for pat in &patterns {
        if let Some(caps) = regex::Regex::new(pat).ok()?.captures(content) {
            if let Some(m) = caps.get(1) {
                if !m.as_str().trim().is_empty() {
                    return Some(m.as_str().trim().to_string());
                }
            }
        }
    }
    None
}
```

### ResponseMessage 结构体扩展

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ResponseMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default)]              // 新增：OpenRouter 统一格式
    pub reasoning_details: Option<Vec<ReasoningDetail>>,
    #[serde(skip_serializing)]     // 新增：非标准字段，skip API 序列化
    pub reasoning: Option<String>,  // 4 层提取后的统一结果
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReasoningDetail {
    #[serde(rename = "type")]
    pub detail_type: String,
    pub summary: Option<String>,
}
```

### agent.rs 集成

```rust
// Agent 结构体新增字段
pub on_reasoning: Option<Arc<Mutex<Box<dyn FnMut(&str) + Send>>>>,

// API 响应后，存储到 message 并回调
if let Some(reasoning) = extract_reasoning(&assistant_msg) {
    let mut msg_for_history = assistant_msg.clone();
    msg_for_history = msg_for_history.with_reasoning(reasoning.clone());
    messages.push(msg_for_history);
    
    if let Some(ref cb) = self.on_reasoning {
        cb.lock().unwrap()(&reasoning);
    }
}
```

### 设计决策

| 决策 | 选择 | 原因 |
|------|------|------|
| 提取逻辑放哪 | `llm.rs` 内部 | `ResponseMessage` 是 llm 模块的类型 |
| 回调放哪 | `Agent` 结构体 | TUI 是 main.rs 的事，agent 不关心 UI |
| reasoning 是否存入 message | 是，`#[serde(skip_serializing)]` | 调试有用，不污染 API 请求 |
| 正则库 | 内置 `regex`（已有依赖） | 不需要新 crate |
| 流式 scrubber | Phase 2 不实现 | 流式推理标签跨 chunk 是边缘 case，先做非流式版本 |

### 改动文件

| 文件 | 改动 |
|------|------|
| `src/models.rs` | `ResponseMessage` 加 `reasoning_details`、`reasoning` 字段；加 `ReasoningDetail` 结构体 |
| `src/llm.rs` | 加 `extract_reasoning()` + `extract_inline_xml_reasoning()` |
| `src/agent.rs` | 加 `on_reasoning` 字段 + 回调触发（~15 行） |

---

## Phase 3：工具调用防护栏（Guardrails）

### 问题

当前工具处理的三个薄弱点：

1. **工具名**：不在 registry 中直接报错，不尝试修复
2. **JSON 参数**：解析失败静默吞掉，用空对象 `{}` 替代
3. **连续失败**：同一工具反复失败不阻断，浪费迭代预算

### 方案：四层防护管道

```
Tool Call Pipeline
  │
  ├─ Layer 1: 工具名模糊修复（Fuzzy Match）
  │     ↓ 修复成功 → 正常执行
  │     ↓ 修复失败 → 返回明确错误
  │
  ├─ Layer 2: JSON 截断检测（Truncation Guard）
  │     ↓ 完整 → 正常执行
  │     ↓ 截断 → 返回错误，不执行
  │
  ├─ Layer 3: 连续失败冷却（Circuit Breaker）
  │     ↓ 未触发 → 正常执行
  │     ↓ 触发 → 跳过，返回冷却提示
  │
  └─ Layer 4: 重复去重（Dedup）
        ↓ 重复 → 跳过，复用首次结果
        ↓ 不重复 → 正常执行
```

### Layer 1：工具名模糊修复

```rust
fn normalize_tool_name(name: &str) -> String {
    name.to_lowercase()
        .replace('-', "_")
        .replace('.', "_")
        .replace(' ', "_")
}

fn fuzzy_match_tool(input: &str, registry: &ToolRegistry) -> Option<String> {
    let normalized = normalize_tool_name(input);
    let all_names = registry.all_tool_names();
    
    // 1. 精确匹配（归一化后）
    if let Some(name) = all_names.iter().find(|n| *n == &normalized) {
        return Some(name.clone());
    }
    
    // 2. 前缀匹配
    for name in &all_names {
        if name.starts_with(&normalized) || normalized.starts_with(name) {
            return Some(name.clone());
        }
    }
    
    // 3. 编辑距离匹配（编辑距离 <= 3）
    let mut best: Option<(usize, &String)> = None;
    for name in &all_names {
        let dist = edit_distance(name, &normalized);
        if dist <= 3 {
            match best {
                Some((d, _)) if dist < d => best = Some((dist, name)),
                None => best = Some((dist, name)),
                _ => {}
            }
        }
    }
    best.map(|(_, n)| n.clone())
}

/// 简单 Levenshtein 编辑距离（~15 行）
fn edit_distance(a: &str, b: &str) -> usize { ... }
```

**匹配示例：**

| 模型输出 | 实际注册名 | 结果 |
|---------|-----------|------|
| `mcp_filesystem_readFile` | `mcp_filesystem_read_file` | 归一化精确匹配 |
| `Read-File` | `read_file` | 归一化匹配 |
| `read` | `read_file` | 前缀匹配 |
| `totally_bogus` | — | 无匹配 |

### Layer 2：JSON 截断检测

```rust
fn is_truncated_json(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s == "{}" || s == "[]" { return false; }
    
    // 括号深度
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for c in s.chars() {
        if escaped { escaped = false; continue; }
        if c == '\\' && in_string { escaped = true; continue; }
        if c == '"' { in_string = !in_string; continue; }
        if !in_string {
            match c { '{' | '[' => depth += 1, '}' | ']' => depth -= 1, _ => {} }
        }
    }
    
    // 未闭合 或 以分隔符结尾
    depth != 0 || s.ends_with(',') || s.ends_with(':')
        || s.ends_with('{') || s.ends_with('[') || s.ends_with('\\')
}
```

### Layer 3：连续失败冷却

```rust
pub struct ToolGuardrail {
    failures: Mutex<HashMap<String, (Instant, usize)>>,
    fail_threshold: usize,     // 默认 3
    block_secs: u64,            // 默认 30
    cooldown_secs: u64,         // 默认 10（窗口重置期）
}

impl ToolGuardrail {
    pub fn should_block(&self, tool_name: &str) -> Option<u64> {
        let mut failures = self.failures.lock().unwrap();
        if let Some((last_fail, count)) = failures.get(tool_name) {
            if *count >= self.fail_threshold {
                let elapsed = last_fail.elapsed().as_secs();
                if elapsed < self.block_secs {
                    return Some(self.block_secs - elapsed);
                }
                failures.remove(tool_name); // 冷却到期
            }
        }
        None
    }
    
    pub fn record_success(&self, tool_name: &str) {
        self.failures.lock().unwrap().remove(tool_name);
    }
    
    pub fn record_failure(&self, tool_name: &str) {
        let mut failures = self.failures.lock().unwrap();
        let entry = failures.entry(tool_name.to_string()).or_default();
        if entry.0.elapsed().as_secs() > self.cooldown_secs {
            *entry = (Instant::now(), 1);
        } else {
            entry.1 += 1;
            entry.0 = Instant::now();
        }
    }
}
```

### Layer 4：重复去重

同一轮迭代内，对同一个 tool_call 的相同调用（tool_name + args 字符串相同），只执行一次：

```rust
// 在 Agent 结构体中
dedup_cache: Mutex<HashMap<String, String>>,  // args_hash → result

// 执行前检查
let cache_key = format!("{}:{}", tool_name, args_str);
if let Some(cached) = self.dedup_cache.lock().unwrap().get(&cache_key) {
    messages.push(Message::tool(&tc.id, tool_name, cached.clone()));
    continue;
}
// ... 执行工具 ...
self.dedup_cache.lock().unwrap().insert(cache_key, result.clone());
```

### 集成到 agent.rs 工具处理段

```rust
for tc in &valid_tool_calls {
    // Layer 1: 模糊修复
    let resolved_name = fuzzy_match_tool(&tc.function.name, &self.registry)
        .unwrap_or_else(|| tc.function.name.clone());
    
    // Layer 2: JSON 截断检测
    if is_truncated_json(&tc.function.arguments) {
        messages.push(Message::tool(&tc.id, &resolved_name,
            "Error: arguments appear truncated (unclosed JSON). Please retry.".into()));
        continue;
    }
    
    // Layer 3: 冷却检查
    if let Some(remaining) = self.guardrail.should_block(&resolved_name) {
        messages.push(Message::tool(&tc.id, &resolved_name,
            format!("Tool '{}' cooling down. Retry in {}s.", resolved_name, remaining)));
        continue;
    }
    
    // Layer 4: 去重
    let dedup_key = format!("{}:{}", resolved_name, tc.function.arguments);
    if let Some(cached) = self.dedup_cache.lock().unwrap().get(&dedup_key) {
        messages.push(Message::tool(&tc.id, &resolved_name, cached.clone()));
        continue;
    }
    
    // 执行
    let result = self.execute_tool_call(&tc);
    match result {
        Ok(content) => {
            self.guardrail.record_success(&resolved_name);
            self.dedup_cache.lock().unwrap().insert(dedup_key, content.clone());
            messages.push(Message::tool(&tc.id, &resolved_name, content));
        }
        Err(e) => {
            self.guardrail.record_failure(&resolved_name);
            messages.push(Message::tool(&tc.id, &resolved_name,
                format!("{{\"error\": \"{}\"}}", e)));
        }
    }
}
```

### 改动文件

| 文件 | 改动 |
|------|------|
| `src/guardrails.rs` | **新建** — `ToolGuardrail`、`fuzzy_match_tool`、`is_truncated_json`、`edit_distance` |
| `src/agent.rs` | 加 `guardrail` 和 `dedup_cache` 字段；改造工具处理段 |
| `src/models.rs` | 无需改动 |
| `src/tool_registry.rs` | 加 `all_tool_names()` 方法（一行） |

---

## 非目标（明确不做）

| 特性 | 原因 |
|------|------|
| LLM 辅助上下文压缩 | mini-agent 目标轻量，压缩可作为未来独立 Phase |
| Provider fallback chains | 目标平台场景可控 |
| 流式推理 scrubber | 边缘 case，Phase 2 先做非流式版本 |
| 外部 MemoryProvider 插件系统 | MemoryProvider trait 已存在，已有扩展能力，不需要额外工程 |
| 工具并发执行 | Phase 1-3 都零新依赖；并发需 `std::thread::spawn`，可在 Phase 3 之后独立评估 |

---

## 依赖影响

| 新增 crate | 原因 | 可选替代 |
|-----------|------|---------|
| 无 | 所有功能用已有依赖（`regex`、`chrono`、`rusqlite`）实现 | — |

---

## 实施顺序建议

```
Phase 1: Working Memory    → src/memory.rs（1 个文件，~100 行新代码）
Phase 2: Reasoning 提取    → src/llm.rs + src/models.rs（2 个文件，~60 行新代码）
Phase 3: Guardrails        → src/guardrails.rs + src/agent.rs + src/tool_registry.rs（3 个文件，~120 行新代码）
```

每个 Phase 独立可编译、可测试。建议按顺序实现，每 Phase 完成后 commit。
