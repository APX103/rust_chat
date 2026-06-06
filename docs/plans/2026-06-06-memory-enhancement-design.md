# Memory System Enhancement Design

**Date**: 2026-06-06
**Project**: rust_chat (mini-agent)
**Inspiration**: hermes-agent memory architecture

## 1. Goal

Enhance rust_chat's agent memory system with three core features from hermes-agent:

1. **File Memory Layer** — LLM-self-writable Markdown memory files (MEMORY.md + USER.md)
2. **Context Compression Engine** — 5-phase automatic compression to prevent context window overflow
3. **Session Search** — SQLite + FTS5 full-text search across all sessions

## 2. Architecture

```
LAYER 0: WORKING MEMORY (existing)
  Agent.conversation_history — in-memory, capped at 50 messages

LAYER 1: FILE MEMORY (new)
  MEMORY.md + USER.md — §-delimited Markdown, LLM self-writable
  · Security scanning (injection/exfil patterns)
  · Concurrency lock + atomic writes
  · Frozen snapshot injected into system prompt

LAYER 2: EPISODIC + SEMANTIC + PROCEDURAL + USER PROFILE (existing)
  SQLite four-layer storage

LAYER 3: CONTEXT COMPRESSION (new)
  5-phase: tool result pruning → protect head/tail → summarize middle
  · Anti-thrashing (<10% savings → skip)
  · Failure fallback
  · Incremental summary updates

LAYER 4: SESSION SEARCH (new)
  sessions + messages + messages_fts tables
  Three modes: Discovery / Scroll / Browse
```

## 3. File Memory Layer

### 3.1 Data Model

- **MEMORY.md** — Agent's own memory notes, default 2200 char limit
- **USER.md** — User profile/preferences, default 1375 char limit
- Entries delimited by `\n§\n`

### 3.2 Key Behaviors

- **Dual state**: Frozen `_system_prompt_snapshot` at load time (never mutated mid-session to preserve prefix cache), live `memory_entries` mutated by tool calls
- **Atomic writes**: Write to temp file → `rename` to replace (no in-place mutation)
- **Security scanning**: Regex-based injection/exfiltration detection at load time; blocked entries replaced with `[BLOCKED: ...]` placeholders
- **Prefix cache protection**: System prompt rebuilt only on context compression, not per-turn
- **External drift detection**: If on-disk file content can't round-trip through parser, save `.bak.<timestamp>` and refuse mutation

### 3.3 Tool Interface

```json
{ "action": "add" | "replace" | "remove", "target": "memory" | "user", "content": "...", "old_text": "..." }
```

### 3.4 New Files

- `src/file_memory.rs` — `FileMemoryStore` struct with load/save/CRUD operations

### 3.5 Modified Files

- `src/models.rs` — Add `FileMemoryConfig`
- `src/memory.rs` — Integrate `FileMemoryStore` into `MemoryManager`
- `src/main.rs` — Initialize and register tools

## 4. Context Compression Engine

### 4.1 5-Phase Algorithm

| Phase | Operation | LLM Cost |
|-------|-----------|----------|
| 1. Tool result pruning | Old tool outputs → 1-line summaries; dedup identical results | None |
| 2. Protect head | Keep system prompt + first N messages | None |
| 3. Protect tail | Keep recent M messages by token budget | None |
| 4. Summarize middle | LLM-generated structured summary of middle turns | 1 call |
| 5. Sanitize | Fix orphaned tool_call/result pairs; strip old images | None |

### 4.2 Summary Template (structured)

```
## Active Task
## Completed Actions
## Key Decisions
## Pending User Asks
## Relevant Files
## Remaining Work
```

### 4.3 Protection Mechanisms

- **Anti-thrashing**: If last 2 compressions each saved <10%, skip
- **Failure fallback**: Deterministic fallback with locally-extracted continuity anchors
- **Incremental updates**: Re-compaction preserves previous summary, appends updates
- **Cooldown**: 30-60s transient failure, 600s no-provider

### 4.4 New Files

- `src/compression.rs` — `ContextCompressor` with `compress()` method

### 4.5 Modified Files

- `src/agent.rs` — Compression trigger in `run_conversation()` loop
- `src/models.rs` — Add `CompressionConfig`

## 5. Session Search

### 5.1 Schema

```sql
sessions: id, title, started_at, ended_at, parent_session_id, message_count, total_tokens
messages: id, session_id, role, content, tool_calls, tool_call_id, timestamp
messages_fts: FTS5 virtual table on (content)
```

### 5.2 Three Search Modes

| Mode | Parameters | Returns | Use Case |
|------|-----------|---------|----------|
| Discovery | query | Matching sessions with snippets | Find by topic |
| Scroll | session_id + message_id | Message window | Paginate through |
| Browse | none | Recent sessions list | Overview of history |

### 5.3 Integration Points

- Store every turn's messages to `messages` table
- Register `session_search` tool for LLM-initiated recall
- Prefetch relevant sessions before each LLM call

### 5.4 New Files

- `src/session_search.rs` — `SessionDB` struct and `session_search` tool handler

### 5.5 Modified Files

- `src/main.rs` — Initialize SessionDB, register tool
- `src/memory.rs` — Pass session_id to providers for message storage

## 6. Configuration Extension

```toml
[file_memory]
enabled = true
memory_char_limit = 2200
user_char_limit = 1375

[compression]
enabled = true
threshold_percent = 0.50
protect_first_n = 3
protect_last_n = 20
summary_target_ratio = 0.20
abort_on_summary_failure = false

[context]
session_search_enabled = true
max_sessions_to_index = 100
```

## 7. Implementation Order

1. **File Memory** — Foundation layer, enables LLM self-management
2. **Session Search** — Infrastructure for long-term recall
3. **Context Compression** — Safety net for context overflow, depends on file memory for system prompt rebuild
