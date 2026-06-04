# Mini-Agent Architecture

This document describes how Mini-Agent inherits and simplifies key design patterns from [Hermes Agent](https://github.com/NousResearch/hermes-agent).

## Design Philosophy

> "Mini is the software size, not the architecture."

Mini-Agent preserves the **core architectural patterns** of Hermes Agent while eliminating heavy dependencies, platform-specific code, and niche features. The result is a single static binary (~5-15MB) that runs on ARM64 + Debian 7.

## Inherited from Hermes Agent

### 1. Multi-Layer Memory System

**Hermes design:** `MemoryManager` orchestrates `MemoryProvider` plugins. Only one external provider is active at a time. Built-in `MemoryStore` provides frozen snapshots.

**Mini-Agent adaptation:**

```
MemoryManager (orchestrator)
└── BuiltinMemoryProvider (SQLite-backed)
    ├── Working Memory    → in-memory conversation context
    ├── Episodic Memory   → turns table + auto-summarization
    ├── Semantic Memory   → semantic_memory table (keyword search)
    ├── Procedural Memory → procedural_memory table (tool usage stats)
    └── User Profile      → user_profile table (preferences + confidence)
```

**Key behaviors inherited:**
- `prefetch_all(query)` — recall before each API call
- `sync_turn(user, assistant)` — persist after each turn
- `on_turn_start(turn_number, message)` — cadence tracking
- `build_memory_context_block()` — Hermes-style `<memory-context>` fencing
- Auto-extract facts ("I like...", "I prefer...") to semantic memory

**Future extensibility:** New memory backends can implement the `MemoryProvider` trait.

### 2. ReAct (Reasoning + Acting) Loop

**Hermes design:** `agent/conversation_loop.py` (~4700 lines) with budget control, retry logic, fallback chains, reasoning management, and guardrails.

**Mini-Agent simplification:**

```rust
while (api_call_count < max_iterations && budget.remaining() > 0) || budget_grace_call {
    // 1. Budget check (IterationBudget.consume/refund)
    // 2. Memory prefetch + inject into messages
    // 3. LLM API call with tools
    // 4. If tool_calls → validate → execute → append results → continue
    // 5. If no tool_calls → final response → break
}
```

**Preserved from Hermes:**
- Budget-controlled iteration (`IterationBudget` with consume/refund)
- Grace call on budget exhaustion
- Tool call validation (name exists, JSON args valid)
- Memory prefetch → LLM → sync cycle
- Conversation history maintenance

**Removed:** Provider fallback chains, streaming, reasoning echo-back, compression, checkpointing, guardrails.

### 3. MCP (Model Context Protocol) Client

**Hermes design:** `tools/mcp_tool.py` (~3800 lines) with stdio/HTTP/SSE transports, background asyncio event loop, circuit breaker, OAuth, sampling, dynamic tool refresh.

**Mini-Agent simplification:**

- **Transport:** stdio (subprocess stdin/stdout) and HTTP (POST JSON-RPC)
- **Protocol:** Lightweight JSON-RPC 2.0 client
- **Tool registration:** `mcp_{server}_{tool}` naming convention (same as Hermes)
- **Schema normalization:** Missing `type` → `"object"`, `$defs` → `definitions`
- **Security:** Filtered environment for stdio subprocesses (PATH, HOME, etc. only)

**Removed:** SSE transport, circuit breaker, OAuth, sampling, dynamic refresh, background event loop (uses threads instead).

### 4. Skill System

**Hermes design:** Three-layer storage (`skills/` bundled, `optional-skills/`, `~/.hermes/skills/` runtime), SKILL.md with YAML frontmatter, `skill_manage` tool, security scanning, curator integration.

**Mini-Agent simplification:**

- **Storage:** Single directory `~/.mini-agent/skills/`
- **Format:** TOML frontmatter + Markdown body
- **CRUD:** `skill_manage` tool with actions: create, update, patch, delete
- **Discovery:** `skills_list` and `skill_view` tools
- **Invocation:** `/skill-name` slash command in REPL

**Preserved from Hermes:**
- Skill content injected as **user message** (not system prompt) — preserves prompt caching
- Frontmatter metadata (name, description, version, tags, triggers)
- Agent can self-manage skills via tool calls

**Removed:** Security scanning, quarantine, hub/lockfile, manifest sync, curator.

### 5. Tool Registry

**Hermes design:** `tools/registry.py` with `ToolRegistry` singleton, lazy auto-discovery, toolset grouping, parallel-safe annotations.

**Mini-Agent adaptation:**

```rust
pub struct ToolRegistry {
    tools: Mutex<HashMap<String, Tool>>,
}
```

- Unified dispatch: `registry.dispatch(name, args)` — no distinction between builtin/MCP/skill tools
- Tool source tracking: `ToolSource::Builtin | Mcp { server } | Skill { skill }`
- Schema collection for LLM API: `registry.list_tools()`

## Message Format

OpenAI Chat Completions format is the internal lingua franca, same as Hermes:

```json
{"role": "system", "content": "..."}
{"role": "user", "content": "..."}
{"role": "assistant", "content": "...", "tool_calls": [...]}
{"role": "tool", "tool_call_id": "...", "name": "...", "content": "..."}
```

Internal fields (not sent to API):
- `message.reasoning` — extracted reasoning text

## Data Flow per Turn

```
User Input
    │
    ▼
┌─────────────────┐
│ on_turn_start() │  ← notify memory providers
└─────────────────┘
    │
    ▼
┌─────────────────┐
│ prefetch_all()  │  ← recall from all memory layers
└─────────────────┘
    │
    ▼
┌─────────────────────────────┐
│ Inject <memory-context>     │  ← into user message
│ + system prompt + history   │
└─────────────────────────────┘
    │
    ▼
┌─────────────────┐
│ LLM API Call    │  ← with available tool schemas
└─────────────────┘
    │
    ├──→ tool_calls ──→ execute ──→ append results ──→ loop
    │
    └──→ final_response ──→ break
    │
    ▼
┌─────────────────┐
│ sync_all()      │  ← persist to SQLite memory
└─────────────────┘
    │
    ▼
┌─────────────────┐
│ Return response │
└─────────────────┘
```

## Target Platform: ARM64 + Debian 7

Debian 7 (Wheezy) ships glibc 2.13, which is too old for modern Rust binaries. Solution: **static musl linking**.

```bash
# Build produces a fully static binary with zero dynamic dependencies
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl

# Verification:
file target/aarch64-unknown-linux-musl/release/mini-agent
# → ELF 64-bit LSB executable, ARM aarch64, version 1 (SYSV), statically linked

ldd target/aarch64-unknown-linux-musl/release/mini-agent
# → not a dynamic executable
```

### Dependency choices for compatibility

| Component | Choice | Reason |
|-----------|--------|--------|
| HTTP client | `ureq` | Pure Rust, lightweight, no async runtime |
| SQLite | `rusqlite` + `bundled` | Ships SQLite C code, no system dependency |
| JSON | `serde` + `serde_json` | Standard, zero C dependencies |
| Async | Not used | Threads are sufficient and smaller |
| TLS | `rustls` via `ureq` | Pure Rust, no OpenSSL version issues |

## Future Extension Points

1. **New MemoryProvider:** Implement trait, register with `MemoryManager::add_provider()`
2. **New transport:** Add variant to `McpTransport`, implement `request()`
3. **New tool:** Call `registry.register_tool()` from any module
4. **Skill triggers:** Add regex/keyword matching in `SkillManifest.triggers`
5. **Embedding search:** Replace keyword search with lightweight ONNX embeddings
