# Mini-Agent

A minimal but powerful AI agent written in Rust, inspired by [Hermes Agent](https://github.com/NousResearch/hermes-agent).

## Features

- **Multi-Layer Memory System** (inherited from Hermes)
  - **Working Memory**: In-memory conversation context
  - **Episodic Memory**: Conversation history with auto-summarization (SQLite)
  - **Semantic Memory**: Hybrid FTS5 full-text search + weighted scoring (SQLite)
  - **Procedural Memory**: Tool usage patterns and skill preferences
  - **User Profile**: Persistent user preferences with confidence scores
  - Configurable provider (`builtin` / `none`) and hybrid search toggle

- **MCP (Model Context Protocol) Client**
  - Connect to stdio-based MCP servers (npx, uvx, etc.)
  - Connect to HTTP/SSE-based MCP servers
  - Automatic tool discovery and registration
  - Graceful shutdown and error handling

- **Self-Managing Skills**
  - Agent can create, update, patch, and delete skills via `skill_manage` tool
  - Skill invocation with `/skill-name` syntax
  - TOML frontmatter for skill metadata

- **ReAct Reasoning-Acting Loop**
  - Budget-controlled iteration (`max_iterations`)
  - Tool call validation and error recovery
  - Memory prefetch before each turn, sync after each turn
  - Grace call on budget exhaustion

- **OpenAI-Compatible API**
  - Works with OpenAI, OpenRouter, local vLLM/Ollama, Azure, Kimi, DeepSeek, etc.
  - Supports custom base URL, API key, temperature, top_p, max_tokens, timeout
  - Supports extra headers (e.g., OpenRouter's HTTP-Referer, X-Title)
  - Supports reasoning content (DeepSeek, Kimi, etc.)

- **Observability** (ZeroClaw-inspired)
  - `Observer` trait with event types: `LlmRequest`, `LlmResponse`, `ToolCall`, `ToolResult`, `MemoryWrite`, `TurnComplete`
  - Default `LogObserver` — structured event logging
  - Configurable: `log` or `noop`

- **Heartbeat Background Tasks** (ZeroClaw-inspired)
  - Periodic auto-summarize, memory cleanup, profile reports
  - Runs in a background thread, no async runtime needed
  - Configurable interval and task list

- **Identity Configuration** (ZeroClaw-inspired)
  - Load agent personality from `~/.mini-agent/identity.md` (OpenClaw-style) or `.json`
  - Decouples agent personality from code
  - Supports name, description, personality traits, rules, notes

## Target Platform

Optimized for **ARM64 (Cortex-A57) + Debian 7 (Wheezy)** via static musl linking.

## Build

### Native Build

```bash
cd mini-agent
cargo build --release
```

### Cross-Compile for ARM64 + musl (Debian 7 compatible)

```bash
# Install target
rustup target add aarch64-unknown-linux-musl

# Install cross-compiler (on Debian/Ubuntu)
sudo apt-get install gcc-aarch64-linux-gnu

# Configure linker (in .cargo/config.toml or environment)
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc

# Build static binary
cargo build --release --target aarch64-unknown-linux-musl

# Result: target/aarch64-unknown-linux-musl/release/mini-agent
# This binary has NO dynamic dependencies and runs on Debian 7.
```

### Verify Static Linking

```bash
file target/aarch64-unknown-linux-musl/release/mini-agent
# Should show: statically linked

ldd target/aarch64-unknown-linux-musl/release/mini-agent
# Should show: not a dynamic executable
```

## Configuration

Create `~/.mini-agent/config.toml`:

```toml
# OpenAI
[model]
provider = "openai"
model = "gpt-4o-mini"
api_key = "sk-..."
base_url = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.7

# OpenRouter (聚合多种模型)
# [model]
# provider = "openrouter"
# model = "anthropic/claude-3.5-sonnet"
# api_key = "sk-or-v1-..."
# base_url = "https://openrouter.ai/api/v1"
# extra_headers = { HTTP-Referer = "https://your-app.com", X-Title = "Mini-Agent" }

# 本地 Ollama / vLLM (无需 API Key)
# [model]
# provider = "ollama"
# model = "qwen2.5:14b"
# api_key = ""
# base_url = "http://localhost:11434/v1"

[memory]
enabled = true
semantic_search_top_k = 5
episodic_summary_threshold = 10

[agent]
max_iterations = 30
enable_reasoning = true

[mcp_servers]
[mcp_servers.time]
command = "uvx"
args = ["mcp-server-time"]

[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
timeout = 30
```

## Usage

```bash
# First run — interactive setup wizard
./mini-agent --setup

# Run
./mini-agent

# Inside the REPL:
🧠 You: Hello!
🤖 Agent: Hello! How can I help you today?

# Use a skill
🧠 You: /my-skill

# Manage skills via natural language
🧠 You: Create a skill for analyzing log files
🤖 Agent: [uses skill_manage tool to create the skill]

# Exit
🧠 You: /quit
```

## Architecture

```
mini-agent/
├── src/
│   ├── main.rs           # CLI entry, REPL loop, onboarding wizard
│   ├── agent.rs          # ReAct core loop + observer instrumentation
│   ├── memory.rs         # Multi-layer memory (SQLite + FTS5 hybrid)
│   ├── mcp.rs            # MCP client (stdio/HTTP)
│   ├── skill.rs          # Skill CRUD + invocation
│   ├── llm.rs            # OpenAI-compatible API client
│   ├── config.rs         # Config loading (~/.mini-agent/config.toml)
│   ├── models.rs         # Shared data types + config structs
│   ├── tool_registry.rs  # Tool dispatch
│   ├── observer.rs       # Observability trait + LogObserver
│   ├── heartbeat.rs      # Background task scheduler
│   └── identity.rs       # Agent personality config
├── systemd/
│   └── mini-agent.service   # systemd service template
```

## Memory System Design

Inspired by Hermes Agent's `MemoryManager` + `MemoryProvider` architecture:

```
MemoryManager
├── BuiltinMemoryProvider (SQLite)
│   ├── Working: In-memory current context
│   ├── Episodic: turns table (auto-summarize every N turns)
│   ├── Semantic: semantic_memory + semantic_memory_fts (FTS5 hybrid)
│   ├── Procedural: procedural_memory table (tool usage stats)
│   └── User Profile: user_profile table (preferences)
└── [Future: External memory providers via trait]
```

Lifecycle per turn:
1. `on_turn_start()` — notify providers
2. `prefetch_all(query)` — recall relevant context
3. Inject `<memory-context>` into user message
4. LLM API call
5. `sync_all(user, assistant)` — persist turn
6. Auto-extract facts to semantic memory

## License

MIT
