# Mini-Agent

一个用 Rust 编写的极简但强大的 AI Agent，灵感来自 [Hermes Agent](https://github.com/NousResearch/hermes-agent)。

## 核心功能

- **多层记忆系统**（继承自 Hermes）
  - **工作记忆**：内存中的对话上下文
  - **情景记忆**：带自动摘要的对话历史（SQLite）
  - **语义记忆**：FTS5 全文检索 + 加权评分（SQLite）
  - **程序记忆**：工具使用模式和技能偏好
  - **用户画像**：带置信度打分的持久化用户偏好
  - 可配置 Provider（`builtin` / `none`）和混合搜索开关

- **MCP（模型上下文协议）客户端**
  - 连接基于 stdio 的 MCP 服务器
  - 连接基于 HTTP/SSE 的 MCP 服务器
  - 自动发现工具并注册
  - 优雅关闭和错误处理

- **自管理技能**
  - Agent 可以通过 `skill_manage` 工具创建、更新、删除技能
  - 使用 `/skill-name` 语法调用技能
  - TOML 前置元数据管理技能

- **ReAct 推理-行动循环**
  - 预算控制的迭代（`max_iterations`）
  - 工具调用验证和错误恢复
  - 每轮前预取记忆，每轮后同步
  - 预算耗尽时优雅收尾

- **OpenAI 兼容 API**
  - 支持 OpenAI、OpenRouter、本地 vLLM/Ollama、Azure、Kimi、DeepSeek 等
  - 支持自定义 base URL、API key、temperature、top_p、max_tokens、timeout
  - 支持额外请求头（如 OpenRouter 的 HTTP-Referer、X-Title）
  - 支持推理内容（DeepSeek、Kimi 等）

- **可观测性**（受 ZeroClaw 启发）
  - `Observer` 事件 trait：`LlmRequest`、`LlmResponse`、`ToolCall`、`ToolResult`、`MemoryWrite`、`TurnComplete`
  - 默认 `LogObserver` —— 结构化事件日志
  - 可配置：`log` 或 `noop`

- **后台心跳任务**（受 ZeroClaw 启发）
  - 周期性自动摘要、记忆清理、画像报告
  - 在后台线程运行，无需异步运行时
  - 可配置间隔和任务列表

- **身份配置**（受 ZeroClaw 启发）
  - 从 `~/.mini-agent/identity.md`（OpenClaw 风格）或 `.json` 加载 Agent 人格
  - 人格与代码解耦
  - 支持姓名、描述、性格特质、规则、备注

## 目标平台

针对 **ARM64 (Cortex-A57) + Debian 7 (Wheezy)** 静态 musl 链接优化。

## 编译

### 本地编译

```bash
cd mini-agent
cargo build --release
```

### 交叉编译到 ARM64 + musl（兼容 Debian 7）

```bash
# 安装目标
rustup target add aarch64-unknown-linux-musl

# 安装交叉编译器（Debian/Ubuntu）
sudo apt-get install gcc-aarch64-linux-gnu

# 配置链接器（在 .cargo/config.toml 或环境变量中）
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc

# 构建静态二进制
cargo build --release --target aarch64-unknown-linux-musl

# 结果：target/aarch64-unknown-linux-musl/release/mini-agent
# 这个二进制没有动态依赖，可在 Debian 7 上运行
```

### 验证静态链接

```bash
file target/aarch64-unknown-linux-musl/release/mini-agent
# 应显示：statically linked

ldd target/aarch64-unknown-linux-musl/release/mini-agent
# 应显示：not a dynamic executable
```

## MCP 工具说明

本项目包含两个**零 Node.js / 零 Python**的 Rust MCP 服务器，通过 stdio 与 mini-agent 通信。

### mcp-time-rs

路径：`examples/mcp-time`

返回当前 UNIX 时间戳，零外部依赖，纯标准库实现。

| 工具名 | 功能 | 参数 |
|--------|------|------|
| `get_current_time` | 返回当前系统 UNIX 时间戳 | 无 |

### mcp-filesystem-rs

路径：`examples/mcp-filesystem`

文件系统操作工具，依赖 `serde_json`。

| 工具名 | 功能 | 参数 |
|--------|------|------|
| `read_file` | 读取文件内容 | `path`: 文件绝对路径 |
| `write_file` | 写入文件 | `path`: 文件绝对路径，`content`: 内容 |
| `list_directory` | 列出目录内容 | `path`: 目录绝对路径 |

### 架构关系

```
mini-agent (主程序)
    │
    │ 通过 stdio 启动并通信
    │ (JSON-RPC 2.0 / MCP)
    ▼
mcp-time-rs          mcp-filesystem-rs
```

mini-agent 会根据配置自动 `spawn()` 启动外部 MCP 进程，通过 stdin/stdout 进行 JSON-RPC 通信。

## 配置

配置搜索顺序（先找到的先生效）：
1. `./.mini-agent/config.toml` —— 当前目录（项目本地）
2. `~/.mini-agent/config.toml` —— 用户主目录（全局）
3. `--config <path>` —— 命令行覆盖

当存在本地配置时，数据目录也会变成本地（`./.mini-agent/data/`）。

### 快速配置

```bash
# 交互式向导（在第一个匹配位置创建配置）
./mini-agent --setup

# 或在当前目录手动创建（开发时推荐）
mkdir .mini-agent
cp config.example.toml .mini-agent/config.toml
# 编辑 .mini-agent/config.toml
```

### 配置 MCP 服务器

在 `.mini-agent/config.toml` 中添加：

```toml
[mcp_servers.time]
command = "/Users/lijialun/work/rust_chat/examples/mcp-time/target/release/mcp-time-rs"
args = []
timeout = 5

[mcp_servers.filesystem]
command = "/Users/lijialun/work/rust_chat/examples/mcp-filesystem/target/release/mcp-filesystem-rs"
args = []
timeout = 10
```

启动 mini-agent 时会自动连接这两个 MCP 服务器， 自动发现 4 个工具：
- `mcp_time_get_current_time`
- `mcp_filesystem_read_file`
- `mcp_filesystem_write_file`
- `mcp_filesystem_list_directory`

### 完整配置示例

```toml
[model]
provider = "openai"
model = "gpt-4o-mini"
api_key = "sk-..."
base_url = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.7
top_p = 1.0
timeout = 120

[memory]
enabled = true
provider = "builtin"
semantic_search_top_k = 5
episodic_summary_threshold = 10
hybrid_search = true

[observer]
enabled = true
kind = "log"

[heartbeat]
enabled = false
interval_secs = 3600
tasks = ["auto_summarize", "memory_cleanup"]

[agent]
max_iterations = 30
enable_reasoning = true

[mcp_servers]

[mcp_servers.time]
command = "/Users/lijialun/work/rust_chat/examples/mcp-time/target/release/mcp-time-rs"
args = []
timeout = 5

[mcp_servers.filesystem]
command = "/Users/lijialun/work/rust_chat/examples/mcp-filesystem/target/release/mcp-filesystem-rs"
args = []
timeout = 10
```

## 使用方式

### 交互式对话

```bash
# 首次运行 —— 交互式配置向导
./mini-agent --setup

# 正常运行
./mini-agent
```

启动后会看到：

```
🔌 Connecting to MCP servers...
✅ MCP connected. Discovered 4 tools.
💡 Type /help for commands, /quit to exit.
```

### 测试 MCP 工具对话

启动后输入以下问题即可触发对应工具：

```
🧠 You: 现在的时间戳是多少？
[LLM 思考...]
🔧 Calling tool: mcp_time_get_current_time({})
✅ Tool mcp_time_get_current_time finished ...
当前 UNIX 时间戳是 1780685xxx

🧠 You: 写一句话到 /tmp/test.txt
[LLM 思考...]
🔧 Calling tool: mcp_filesystem_write_file(...)
✅ Tool mcp_filesystem_write_file finished ...
已写入 /tmp/test.txt

🧠 You: 读一下 /tmp/test.txt
[LLM 思考...]
🔧 Calling tool: mcp_filesystem_read_file(...)
✅ Tool mcp_filesystem_read_file finished ...
文件内容是：...

🧠 You: 列出 /tmp 目录里的文件
[LLM 思考...]
🔧 Calling tool: mcp_filesystem_list_directory(...)
✅ Tool mcp_filesystem_list_directory finished ...
/tmp 目录下有：...
```

### 退出

```
🧠 You: /quit
👋 Goodbye!
```

## 一键测试脚本

项目根目录提供了自动测试脚本：

```bash
python3 test-mcp-dialog.py
```

脚本会自动完成：
1. 编译 `mini-agent`、`mcp-time-rs`、`mcp-filesystem-rs`
2. 临时注释掉需要联网的 MCP（避免测试等待）
3. 启动 mini-agent 并自动进行 4 轮对话测试
4. 验证每个工具是否被正确调用
5. 杀掉进程并恢复原配置

运行成功后会显示：

```
═══ MCP 对话测试报告 ═══
通过: 4
失败: 0

🎉 全部测试通过！MCP time + filesystem 工具已可正常对话调用。
```

### 单独测试 MCP 服务器

如果只想测试 MCP 服务器本身的协议响应，可以直接用 shell：

```bash
# 测试 mcp-time
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | \
  ./examples/mcp-time/target/release/mcp-time-rs

# 测试 mcp-filesystem
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | \
  ./examples/mcp-filesystem/target/release/mcp-filesystem-rs
```

或者进入对应目录运行 Rust 集成测试：

```bash
cd examples/mcp-time
cargo test

cd ../mcp-filesystem
cargo test
```

## 故障排查

### MCP 连接失败

1. 检查二进制是否存在：
   ```bash
   ls examples/mcp-time/target/release/mcp-time-rs
   ls examples/mcp-filesystem/target/release/mcp-filesystem-rs
   ```

2. 检查配置中的路径是否正确：
   ```bash
   grep "command" .mini-agent/config.toml
   ```

3. 单独测试 MCP 服务器响应：
   ```bash
   echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | \
     ./examples/mcp-time/target/release/mcp-time-rs
   ```

### 启动时卡住

如果配置了联网 MCP（如智谱 web-search），启动时会等待网络响应。可以：
- 临时注释掉该 MCP 配置
- 或检查网络连接

### 工具没有被调用

- 确认启动日志显示 `Discovered X tools`，X 应该大于 0
- 提问时尽量明确提到"时间戳"、"写文件"、"读文件"、"列出目录"等关键词
- LLM 可能因 temperature 较高而随机不调用工具，可降低 temperature

## 架构

```
mini-agent/
├── src/
│   ├── main.rs           # CLI 入口、REPL 循环、配置向导
│   ├── agent.rs          # ReAct 核心循环 + 观测器
│   ├── memory.rs         # 多层记忆（SQLite + FTS5 混合）
│   ├── mcp.rs            # MCP 客户端（stdio/HTTP）
│   ├── skill.rs          # 技能增删改查 + 调用
│   ├── llm.rs            # OpenAI 兼容 API 客户端
│   ├── config.rs         # 配置加载（~/.mini-agent/config.toml）
│   ├── models.rs         # 共享数据类型 + 配置结构体
│   ├── tool_registry.rs  # 工具分发
│   ├── observer.rs       # 可观测性 trait + LogObserver
│   ├── heartbeat.rs      # 后台任务调度
│   └── identity.rs       # Agent 人格配置
├── examples/
│   ├── mcp-time/         # 时间 MCP 服务器
│   ├── mcp-filesystem/   # 文件系统 MCP 服务器
│   └── ...
├── systemd/
│   └── mini-agent.service   # systemd 服务模板
└── test-mcp-dialog.py     # MCP 对话一键测试脚本
```

## 记忆系统设计

灵感来自 Hermes Agent 的 `MemoryManager` + `MemoryProvider` 架构：

```
MemoryManager
├── BuiltinMemoryProvider (SQLite)
│   ├── Working: 内存中的当前上下文
│   ├── Episodic: turns 表（每 N 轮自动摘要）
│   ├── Semantic: semantic_memory + semantic_memory_fts（FTS5 混合）
│   ├── Procedural: procedural_memory 表（工具使用统计）
│   └── User Profile: user_profile 表（用户偏好）
└── [未来：通过 trait 接入外部记忆 Provider]
```

每轮生命周期：
1. `on_turn_start()` —— 通知各 Provider
2. `prefetch_all(query)` —— 召回相关上下文
3. 将 `<memory-context>` 注入用户消息
4. 调用 LLM API
5. `sync_all(user, assistant)` —— 持久化本轮
6. 自动提取事实到语义记忆

## 许可证

MIT
