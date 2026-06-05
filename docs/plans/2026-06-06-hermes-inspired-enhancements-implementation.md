# Hermes-Inspired Enhancements Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add three enhancements to mini-agent: Working Memory layer (in-memory ring buffer for recent turns + important facts), Reasoning extraction (4-layer priority chain), and Tool call guardrails (fuzzy matching, JSON truncation detection, circuit breaker, dedup).

**Architecture:** Each feature is a self-contained module. WorkingMemoryProvider plugs into the existing MemoryManager trait system. Reasoning extraction lives in llm.rs. Guardrails live in a new guardrails.rs module. Agent.rs only gains ~30 lines of call-site code per feature. Zero new crate dependencies.

**Tech Stack:** Rust 2021, existing deps (rusqlite, regex, chrono, serde, anyhow, uuid)

---

### Task 1: RingBuffer utility type

**Files:**
- Create: `src/ring_buffer.rs`
- Modify: `src/lib.rs` (or main.rs if no lib)
- Test: `src/ring_buffer.rs` (inline tests)

**Step 1: Check project structure for module exports**

Run: `ls src/*.rs`
Expected: Shows agent.rs, llm.rs, memory.rs, models.rs, mcp.rs, tool_registry.rs, skill.rs, observer.rs, heartbeat.rs, identity.rs, main.rs

**Step 2: Check how modules are declared**

Run: `head -30 src/main.rs`
Expected: See `mod agent;` etc. or `pub mod` declarations

**Step 3: Write ring_buffer.rs**

```rust
//! Fixed-capacity ring buffer for Working Memory.

use std::collections::VecDeque;

/// A fixed-capacity ring buffer that auto-evicts oldest items when full.
#[derive(Debug, Clone, Default)]
pub struct RingBuffer<T> {
    data: VecDeque<T>,
    capacity: usize,
}

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Push an item. If at capacity, the oldest item is evicted.
    pub fn push(&mut self, item: T) {
        if self.data.len() >= self.capacity {
            self.data.pop_front();
        }
        self.data.push_back(item);
    }

    /// Return the most recent N items (up to all items if N > len).
    pub fn recent(&self, n: usize) -> &[T] {
        let start = self.data.len().saturating_sub(n);
        self.data.range(start..).collect::<Vec<_>>().as_slice()
        // VecDeque doesn't have range indexing that returns &[T], so:
        let skip = self.data.len().saturating_sub(n);
        &self.data.iter().skip(skip).cloned().collect::<Vec<_>>()
    }

    /// Total items currently stored.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Iterate over all items.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.data.iter()
    }
}

// ---- Inline tests ----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_and_len() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(3);
        assert_eq!(buf.len(), 0);
        buf.push(1);
        assert_eq!(buf.len(), 1);
        buf.push(2);
        buf.push(3);
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn test_eviction() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        buf.push(4); // should evict 1
        assert_eq!(buf.len(), 3);
        let vals: Vec<_> = buf.iter().cloned().collect();
        assert_eq!(vals, vec![2, 3, 4]);
    }

    #[test]
    fn test_recent() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(5);
        buf.push(10);
        buf.push(20);
        buf.push(30);
        let recent: Vec<_> = buf.recent(2).iter().cloned().collect();
        assert_eq!(recent, vec![20, 30]);
    }

    #[test]
    fn test_recent_more_than_len() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(5);
        buf.push(1);
        buf.push(2);
        let all: Vec<_> = buf.recent(100).iter().cloned().collect();
        assert_eq!(all, vec![1, 2]);
    }
}
```

**Step 4: Wire up the module**

Run: `grep -n "^mod " src/main.rs | head -5`
Expected: See existing mod declarations

Edit `src/main.rs` to add `mod ring_buffer;` (or if there's a `src/lib.rs`, add it there).

**Step 5: Run tests**

Run: `cargo test ring_buffer`
Expected: All 4 tests pass

**Step 6: Commit**

```bash
git add src/ring_buffer.rs src/main.rs
git commit -m "feat: add RingBuffer utility for Working Memory"
```

---

### Task 2: Data types for Working Memory

**Files:**
- Modify: `src/memory.rs`
- Test: inline in memory.rs

**Step 1: Add types to memory.rs**

Add these types at the top of `src/memory.rs` (after imports, before `MemoryProvider` trait):

```rust
// ---- Working Memory types ----

/// A single conversation turn stored in working memory.
#[derive(Debug, Clone)]
pub struct Turn {
    pub turn_number: usize,
    pub user: String,
    pub assistant: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tool_calls_used: Vec<String>,
}

impl Turn {
    pub fn new(turn_number: usize, user: impl Into<String>, assistant: impl Into<String>) -> Self {
        Self {
            turn_number,
            user: user.into(),
            assistant: assistant.into(),
            timestamp: chrono::Utc::now(),
            tool_calls_used: vec![],
        }
    }
}

/// Source of a memory fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactSource {
    /// User explicitly said "remember this" or similar.
    UserExplicit,
    /// Agent inferred from conversation patterns.
    AgentInferred,
}

/// A fact stored in working memory facts store.
#[derive(Debug, Clone)]
pub struct MemoryFact {
    pub key: String,
    pub value: String,
    pub source: FactSource,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl MemoryFact {
    pub fn new(key: impl Into<String>, value: impl Into<String>, source: FactSource) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            source,
            created_at: chrono::Utc::now(),
        }
    }
}
```

**Step 2: Add WorkingMemoryProvider struct**

Add after the `MemoryManager` impl block (around line 77):

```rust
// ---- Working Memory Provider ----

/// In-memory working memory provider using a ring buffer.
/// Stores recent turns and explicitly marked facts for the current session.
pub struct WorkingMemoryProvider {
    turns: crate::ring_buffer::RingBuffer<Turn>,
    facts: Vec<MemoryFact>,
    max_turns: usize,
    max_facts: usize,
}

impl WorkingMemoryProvider {
    pub fn new(max_turns: usize, max_facts: usize) -> Self {
        Self {
            turns: crate::ring_buffer::RingBuffer::new(max_turns),
            facts: Vec::with_capacity(max_facts),
            max_turns,
            max_facts,
        }
    }

    /// Record a completed turn.
    pub fn record_turn(&mut self, turn: Turn) {
        self.turns.push(turn);
    }

    /// Add an important fact (does not evict; has separate limit).
    pub fn add_fact(&mut self, key: impl Into<String>, value: impl Into<String>, source: FactSource) {
        let key_str = key.into();
        // Update existing if same key
        if let Some(fact) = self.facts.iter_mut().find(|f| f.key == key_str) {
            fact.value = value.into();
            fact.created_at = chrono::Utc::now();
            return;
        }
        // Evict oldest if at capacity
        if self.facts.len() >= self.max_facts {
            self.facts.remove(0);
        }
        self.facts.push(MemoryFact::new(key_str, value, source));
    }

    /// Get recent turns for prefetch output.
    pub fn recent_turns(&self, n: usize) -> Vec<&Turn> {
        // RingBuffer::recent returns a Vec, so we can return references
        let items = self.turns.recent(n);
        items.iter().collect()
    }
}

impl MemoryProvider for WorkingMemoryProvider {
    fn name(&self) -> &str {
        "working"
    }

    fn prefetch(&self, _query: &str, _session_id: &str) -> Result<String> {
        let mut parts = vec![];

        // Recent turns
        if !self.turns.is_empty() {
            let mut lines = vec!["## Recent Conversation (Working Memory)".to_string()];
            for turn in self.recent_turns(self.max_turns) {
                let user_short = truncate(&turn.user, 120);
                let assistant_short = truncate(&turn.assistant, 120);
                lines.push(format!("[turn {}] User: {}", turn.turn_number, user_short));
                lines.push(format!("[turn {}] Assistant: {}", turn.turn_number, assistant_short));
            }
            parts.push(lines.join("\n"));
        }

        // Important facts
        if !self.facts.is_empty() {
            let mut lines = vec!["## Important Facts".to_string()];
            for fact in &self.facts {
                lines.push(format!("- {}: {}", fact.key, fact.value));
            }
            parts.push(lines.join("\n"));
        }

        Ok(parts.join("\n\n"))
    }

    fn sync_turn(&mut self, user: &str, assistant: &str, _session_id: &str) -> Result<()> {
        let turn_number = self.turns.len() + 1;
        self.turns.push(Turn::new(turn_number, user, assistant));
        Ok(())
    }
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len.saturating_sub(3)] // leave room for "..."
    }
}
```

Note: We need to adjust the `RingBuffer::recent` return type. Let's fix Task 1's implementation first:

**Step 2a: Fix RingBuffer::recent to return Vec<&T>**

Edit `src/ring_buffer.rs`, replace the `recent` method:

```rust
    /// Return the most recent N items as owned values.
    pub fn recent_owned(&self, n: usize) -> Vec<T>
    where
        T: Clone,
    {
        let skip = self.data.len().saturating_sub(n);
        self.data.iter().skip(skip).cloned().collect()
    }
```

Actually, let's keep it simpler — return `Vec<T>` requires Clone. For the WorkingMemoryProvider, we can clone Turn since it's small. Let me revise:

```rust
    /// Return the most recent N items (up to all items if N > len).
    pub fn recent_owned(&self, n: usize) -> Vec<T>
    where
        T: Clone,
    {
        let skip = self.data.len().saturating_sub(n);
        self.data.iter().skip(skip).cloned().collect()
    }
```

And in `WorkingMemoryProvider::prefetch`, use `self.turns.recent_owned(self.max_turns)`.

**Step 3: Update RingBuffer tests**

Update the `test_recent` and `test_recent_more_than_len` tests to use `recent_owned`:

```rust
    #[test]
    fn test_recent() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(5);
        buf.push(10);
        buf.push(20);
        buf.push(30);
        let recent = buf.recent_owned(2);
        assert_eq!(recent, vec![20, 30]);
    }
```

**Step 4: Add inline tests to memory.rs**

Add at the bottom of `src/memory.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_working_memory_provider_basic() {
        let mut provider = WorkingMemoryProvider::new(3, 5);
        assert!(provider.prefetch("", "").is_empty()); // empty

        provider.sync_turn("hello", "world", "").unwrap();
        provider.sync_turn("foo", "bar", "").unwrap();

        let ctx = provider.prefetch("", "");
        assert!(ctx.contains("Recent Conversation"));
        assert!(ctx.contains("turn 1"));
        assert!(ctx.contains("turn 2"));
    }

    #[test]
    fn test_working_memory_facts() {
        let mut provider = WorkingMemoryProvider::new(3, 5);
        provider.add_fact("key1", "value1", FactSource::UserExplicit);
        provider.add_fact("key2", "value2", FactSource::AgentInferred);

        let ctx = provider.prefetch("", "");
        assert!(ctx.contains("Important Facts"));
        assert!(ctx.contains("key1: value1"));
        assert!(ctx.contains("key2: value2"));
    }

    #[test]
    fn test_working_memory_fact_update() {
        let mut provider = WorkingMemoryProvider::new(3, 5);
        provider.add_fact("target", "Arduino", FactSource::UserExplicit);
        provider.add_fact("target", "Raspberry Pi", FactSource::UserExplicit);

        let ctx = provider.prefetch("", "");
        // Should have updated, not duplicated
        assert_eq!(ctx.matches("target:").count(), 1);
        assert!(ctx.contains("Raspberry Pi"));
    }

    #[test]
    fn test_ring_buffer_eviction_in_provider() {
        let mut provider = WorkingMemoryProvider::new(2, 5);
        provider.sync_turn("t1_user", "t1_bot", "").unwrap();
        provider.sync_turn("t2_user", "t2_bot", "").unwrap();
        provider.sync_turn("t3_user", "t3_bot", "").unwrap(); // evicts turn 1

        let ctx = provider.prefetch("", "");
        assert!(ctx.contains("turn 2"));
        assert!(ctx.contains("turn 3"));
        assert!(!ctx.contains("turn 1"));
    }
}
```

**Step 5: Run tests**

Run: `cargo test working_memory`
Expected: All 4 tests pass

**Step 6: Commit**

```bash
git add src/ring_buffer.rs src/memory.rs src/main.rs
git commit -m "feat: add Working Memory provider with ring buffer"
```

---

### Task 3: Register WorkingMemoryProvider in main.rs

**Files:**
- Modify: `src/main.rs`

**Step 1: Find where MemoryManager is created**

Run: `grep -n "MemoryManager" src/main.rs`
Expected: See where BuiltinMemoryProvider is registered

**Step 2: Add WorkingMemoryProvider registration**

Add after the BuiltinMemoryProvider registration:

```rust
// Working Memory (in-memory, per-session)
let working_provider = Arc::new(WorkingMemoryProvider::new(10, 20));
memory_manager.add_provider(working_provider);
```

**Step 3: Verify compilation**

Run: `cargo build`
Expected: Compiles successfully

**Step 4: Run all tests**

Run: `cargo test`
Expected: All tests pass

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: register WorkingMemoryProvider in MemoryManager"
```

---

### Task 4: Reasoning extraction — extend ResponseMessage

**Files:**
- Modify: `src/models.rs`

**Step 1: Add ReasoningDetail and extend ResponseMessage**

In `src/models.rs`, after the `ResponseMessage` struct, add:

```rust
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ReasoningDetail {
    #[serde(rename = "type")]
    pub detail_type: String,
    pub summary: Option<String>,
}

// Update ResponseMessage to include reasoning_details:
#[derive(Debug, Clone, Deserialize)]
pub struct ResponseMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub reasoning_details: Option<Vec<ReasoningDetail>>,
    #[serde(skip_serializing)]  // not from API, computed field
    pub reasoning: Option<String>,
}
```

**Step 2: Write tests for deserialization**

Add at bottom of `src/models.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_message_with_reasoning_details() {
        let json = r#"
        {
            "role": "assistant",
            "content": "Here is my answer.",
            "reasoning_details": [
                {"type": "thinking", "summary": "I need to consider X first."},
                {"type": "thinking", "summary": "Then I should check Y."}
            ]
        }
        "#;
        let msg: ResponseMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, Some("Here is my answer.".to_string()));
        assert!(msg.reasoning_details.is_some());
        let details = msg.reasoning_details.unwrap();
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].summary, Some("I need to consider X first.".to_string()));
    }

    #[test]
    fn test_response_message_without_reasoning_fields() {
        let json = r#"{"role": "assistant", "content": "Hello"}"#;
        let msg: ResponseMessage = serde_json::from_str(json).unwrap();
        assert!(msg.reasoning_content.is_none());
        assert!(msg.reasoning_details.is_none());
    }

    #[test]
    fn test_message_with_reasoning_serialized() {
        let msg = Message::assistant("Hello").with_reasoning("thinking process");
        let json = serde_json::to_string(&msg).unwrap();
        // reasoning should be skipped in serialization
        assert!(!json.contains("reasoning"));
    }
}
```

**Step 3: Run tests**

Run: `cargo test models`
Expected: All 3 tests pass

**Step 4: Commit**

```bash
git add src/models.rs
git commit -m "feat: extend ResponseMessage with reasoning_details and reasoning fields"
```

---

### Task 5: Reasoning extraction logic in llm.rs

**Files:**
- Modify: `src/llm.rs`
- Test: inline in llm.rs (or a new `src/llm_tests.rs` if inline is too heavy)

**Step 1: Write the extract_reasoning function**

Add to `src/llm.rs`:

```rust
use regex::Regex;

/// Extract reasoning content from an LLM response using a 4-layer priority chain.
///
/// Priority order:
/// 1. `reasoning` field (DeepSeek, Qwen) — set by API response deserialization
/// 2. `reasoning_content` field (Moonshot, Novita)
/// 3. `reasoning_details` array (OpenRouter unified format)
/// 4. Inline XML tags in content (<think>, <thinking>, <thought>, <reasoning>, <REASONING_SCRATCHPAD>)
pub fn extract_reasoning(msg: &crate::models::ResponseMessage) -> Option<String> {
    // Layer 1: reasoning field (DeepSeek, Qwen)
    if let Some(r) = &msg.reasoning {
        if !r.trim().is_empty() {
            return Some(r.clone());
        }
    }

    // Layer 2: reasoning_content field (Moonshot, Novita)
    if let Some(r) = &msg.reasoning_content {
        if !r.trim().is_empty() {
            return Some(r.clone());
        }
    }

    // Layer 3: reasoning_details array (OpenRouter unified format)
    if let Some(details) = &msg.reasoning_details {
        let summaries: Vec<String> = details
            .iter()
            .filter_map(|d| d.summary.as_ref())
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect();
        if !summaries.is_empty() {
            return Some(summaries.join("\n"));
        }
    }

    // Layer 4: Inline XML tags in content
    extract_inline_xml_reasoning(msg.content.as_deref().unwrap_or(""))
}

/// Extract reasoning from inline XML tags in content.
fn extract_inline_xml_reasoning(content: &str) -> Option<String> {
    static PATTERNS: &[&str] = &[
        r"(?s)<think>(.*?)</think>",
        r"(?s)<thinking>(.*?)</thinking>",
        r"(?s)<thought>(.*?)</thought>",
        r"(?s)<reasoning>(.*?)</reasoning>",
        r"(?s)<REASONING_SCRATCHPAD>(.*?)</REASONING_SCRATCHPAD>",
    ];

    for pattern in PATTERNS {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(content) {
                if let Some(m) = caps.get(1) {
                    let text = m.as_str().trim();
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ResponseMessage;

    fn make_msg(content: &str, reasoning: Option<&str>, reasoning_content: Option<&str>,
                reasoning_details: Option<Vec<(String, Option<String>)>>) -> ResponseMessage {
        ResponseMessage {
            role: "assistant".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            reasoning_content: reasoning_content.map(|s| s.to_string()),
            reasoning_details: reasoning_details.map(|v| v.into_iter().map(|(t, s)| crate::models::ReasoningDetail {
                detail_type: t,
                summary: s,
            }).collect()),
            reasoning: reasoning.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_extract_layer1_reasoning_field() {
        let msg = make_msg("answer", Some("deepseek thought"), None, None);
        assert_eq!(extract_reasoning(&msg), Some("deepseek thought".to_string()));
    }

    #[test]
    fn test_extract_layer2_reasoning_content() {
        let msg = make_msg("answer", None, Some("moonshot thought"), None);
        assert_eq!(extract_reasoning(&msg), Some("moonshot thought".to_string()));
    }

    #[test]
    fn test_extract_layer3_reasoning_details() {
        let details = vec![
            ("thinking".to_string(), Some("step 1".to_string())),
            ("thinking".to_string(), Some("step 2".to_string())),
        ];
        let msg = make_msg("answer", None, None, Some(details));
        assert_eq!(extract_reasoning(&msg), Some("step 1\nstep 2".to_string()));
    }

    #[test]
    fn test_extract_layer4_inline_xml() {
        let msg = make_msg("Answer: 42\n\n<think>Let me think about this...</think>", None, None, None);
        assert_eq!(extract_reasoning(&msg), Some("Let me think about this...".to_string()));
    }

    #[test]
    fn test_extract_layer4_various_tags() {
        for tag in &["think", "thinking", "thought", "reasoning", "REASONING_SCRATCHPAD"] {
            let content = format!("<{}>inner thought</{}>", tag, tag);
            let msg = make_msg(&content, None, None, None);
            assert!(extract_reasoning(&msg).is_some(), "Failed for tag: {}", tag);
        }
    }

    #[test]
    fn test_extract_priority_order() {
        // Layer 1 takes priority over layer 2
        let msg = make_msg("answer", Some("layer1"), Some("layer2"), None);
        assert_eq!(extract_reasoning(&msg), Some("layer1".to_string()));
    }

    #[test]
    fn test_extract_empty() {
        let msg = make_msg("plain answer", None, None, None);
        assert!(extract_reasoning(&msg).is_none());
    }

    #[test]
    fn test_extract_ignores_empty_details() {
        let details = vec![
            ("thinking".to_string(), Some("".to_string())),
            ("thinking".to_string(), None),
        ];
        let msg = make_msg("answer", None, None, Some(details));
        assert!(extract_reasoning(&msg).is_none());
    }
}
```

**Step 2: Run tests**

Run: `cargo test extract_reasoning`
Expected: All 7 tests pass

**Step 3: Commit**

```bash
git add src/llm.rs
git commit -m "feat: add 4-layer reasoning extraction to llm.rs"
```

---

### Task 6: Wire reasoning callback into agent.rs

**Files:**
- Modify: `src/agent.rs`

**Step 1: Add on_reasoning field to Agent struct**

In `src/agent.rs`, add to the `Agent` struct:

```rust
pub on_reasoning: Option<Arc<Mutex<Box<dyn FnMut(&str) + Send>>>>,
```

And in `Agent::new()`, initialize it:

```rust
on_reasoning: None,
```

Add setter:

```rust
pub fn set_on_reasoning(&mut self, f: impl FnMut(&str) + Send + 'static) {
    self.on_reasoning = Some(Arc::new(Mutex::new(Box::new(f))));
}
```

**Step 2: Extract and store reasoning after LLM response**

After the API call block (around line 209, after `total_usage` accumulation), add:

```rust
// Extract reasoning content (Phase 2)
if let Some(reasoning) = crate::llm::extract_reasoning(&assistant_msg) {
    log::debug!("Reasoning ({} chars): {}",
        reasoning.len(),
        &reasoning[..reasoning.len().min(200)]);

    // Store in message for history (not sent to API)
    let mut msg_for_history = assistant_msg.clone();
    msg_for_history.reasoning = Some(reasoning.clone());

    // Notify TUI/observer
    if let Some(ref mut cb) = self.on_reasoning {
        cb.lock().unwrap()(&reasoning);
    }

    // Replace assistant_msg with the enriched version for history
    messages.push(msg_for_history);
} else {
    messages.push(assistant_msg.clone());
}
```

Note: This replaces the later `messages.push(assistant_msg)` calls. We need to refactor slightly: instead of pushing `assistant_msg` at two places (tool call path and final response path), we push the enriched version at the top and work with the `reasoning` field.

Actually, let's be more surgical. The current code pushes `assistant_msg` in two places:
- Line 231: `messages.push(assistant_for_history);` (tool call path)
- Line 299: `messages.push(assistant_msg);` (final response path)

We should extract reasoning once after the API call, store it in the message, then proceed.

**Refactored approach:**

After line 209 (after usage accumulation), before the `if let Some(tool_calls)` block:

```rust
// Extract reasoning and enrich the message for history
let mut assistant_for_history = assistant_msg.clone();
assistant_for_history.reasoning = crate::llm::extract_reasoning(&assistant_msg);

if let Some(ref reasoning) = assistant_for_history.reasoning {
    log::debug!("Reasoning ({} chars): {}",
        reasoning.len(),
        &reasoning[..reasoning.len().min(200)]);
    if let Some(ref mut cb) = self.on_reasoning {
        cb.lock().unwrap()(reasoning);
    }
}
```

Then in the tool call path (line 229-231), replace:
```rust
let mut assistant_for_history = assistant_msg.clone();
assistant_for_history.content = assistant_for_history.content.filter(|c| !c.is_empty());
messages.push(assistant_for_history);
```
with:
```rust
let mut assistant_for_history = assistant_msg.clone();
assistant_for_history.content = assistant_for_history.content.filter(|c| !c.is_empty());
// reasoning already extracted above
messages.push(assistant_for_history);
```

Wait, this creates a double clone. Better approach: extract reasoning once, store in a variable, then use it.

Let me simplify:

```rust
// After API call, before tool_calls check:
let reasoning = crate::llm::extract_reasoning(&assistant_msg);

if let Some(ref r) = reasoning {
    log::debug!("Reasoning: {}...", &r[..r.len().min(200)]);
    if let Some(ref mut cb) = self.on_reasoning {
        cb.lock().unwrap()(r);
    }
}

// In tool_calls path (replace lines 229-231):
let mut assistant_for_history = assistant_msg.clone();
assistant_for_history.content = assistant_for_history.content.filter(|c| !c.is_empty());
assistant_for_history.reasoning = reasoning.clone();  // attach reasoning
messages.push(assistant_for_history);

// In final response path (replace line 299):
messages.push(assistant_msg.with_reasoning(reasoning.unwrap_or_default()));
```

Actually, the cleanest approach: extract once, store in a local, then both branches use it.

**Step 3: Handle the existing reasoning logging**

Remove or replace the existing reasoning log at lines 295-297 (the old single-layer extraction).

**Step 4: Write a quick integration test**

Add to `src/agent.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reasoning_extraction_in_agent() {
        // This is a structural test - verify Agent has on_reasoning field
        // Full integration test requires mocking LlmClient
        let mut agent = Agent::new(
            /* mock client */ unimplemented!(),
            Arc::new(ToolRegistry::new()),
            Arc::new(MemoryManager::new()),
            Arc::new(SkillManager::new()),
            30,
        );
        let received = Arc::new(Mutex::new(None));
        agent.set_on_reasoning({
            let received = received.clone();
            move |r| { *received.lock().unwrap() = Some(r.to_string()); }
        });
        // Verify field exists and is set
        assert!(agent.on_reasoning.is_some());
    }
}
```

Actually, this test is awkward without mocks. Let's skip it and rely on the llm.rs unit tests for reasoning extraction correctness.

**Step 5: Verify compilation**

Run: `cargo build`
Expected: Compiles

**Step 6: Run all tests**

Run: `cargo test`
Expected: All tests pass

**Step 7: Commit**

```bash
git add src/agent.rs src/llm.rs src/models.rs
git commit -m "feat: wire reasoning extraction into agent loop"
```

---

### Task 7: Guardrails module

**Files:**
- Create: `src/guardrails.rs`
- Modify: `src/tool_registry.rs` (add `all_tool_names()`)
- Modify: `src/agent.rs` (integrate guardrail checks)
- Test: inline in guardrails.rs

**Step 1: Write src/guardrails.rs**

```rust
//! Tool call guardrails: fuzzy matching, truncation detection,
//! circuit breaker for repeated failures, and dedup.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::tool_registry::ToolRegistry;

// ---- JSON Truncation Detection ----

/// Check if a JSON string appears to be truncated (unclosed braces/brackets).
pub fn is_truncated_json(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s == "{}" || s == "[]" || s == "null" {
        return false;
    }

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;

    for c in s.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && in_string {
            escaped = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            match c {
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
        }
    }

    depth != 0
        || s.ends_with(',')
        || s.ends_with(':')
        || s.ends_with('{')
        || s.ends_with('[')
        || s.ends_with('\\')
}

// ---- Tool Name Normalization ----

/// Normalize tool name for comparison: lowercase + replace separators with underscore.
pub fn normalize_tool_name(name: &str) -> String {
    name.to_lowercase()
        .replace('-', "_")
        .replace('.', "_")
        .replace(' ', "_")
}

// ---- Fuzzy Tool Name Matching ----

/// Try to find the correct tool name when the model outputs a near-match.
pub fn fuzzy_match_tool(input: &str, registry: &ToolRegistry) -> Option<String> {
    let normalized = normalize_tool_name(input);
    let all_names = registry.all_tool_names();

    // 1. Exact match after normalization
    if let Some(name) = all_names.iter().find(|n| *n == &normalized) {
        return Some(name.clone());
    }

    // 2. Prefix match (handles mcp_ tool name variations)
    for name in &all_names {
        if name.starts_with(&normalized) || normalized.starts_with(name) {
            return Some(name.clone());
        }
    }

    // 3. Contains match + Levenshtein distance (threshold: 3)
    let mut best: Option<(usize, &String)> = None;
    for name in &all_names {
        if name.contains(&normalized) || normalized.contains(name.as_str()) {
            let dist = edit_distance(name, &normalized);
            if dist <= 3 {
                match best {
                    Some((d, _)) if dist < d => best = Some((dist, name)),
                    None => best = Some((dist, name)),
                    _ => {}
                }
            }
        }
    }
    best.map(|(_, n)| n.clone())
}

/// Simple Levenshtein edit distance.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 { return n; }
    if n == 0 { return m; }

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr: Vec<usize> = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

// ---- Circuit Breaker ----

/// Tracks tool failures and blocks repeated calls after a threshold.
pub struct ToolGuardrail {
    failures: Mutex<HashMap<String, (Instant, usize)>>,
    fail_threshold: usize,
    block_secs: u64,
    cooldown_secs: u64,
}

impl ToolGuardrail {
    pub fn new(fail_threshold: usize, block_secs: u64, cooldown_secs: u64) -> Self {
        Self {
            failures: Mutex::new(HashMap::new()),
            fail_threshold,
            block_secs,
            cooldown_secs,
        }
    }

    /// Check if a tool is currently blocked. Returns Some(remaining_seconds) if blocked.
    pub fn should_block(&self, tool_name: &str) -> Option<u64> {
        let mut failures = self.failures.lock().unwrap();
        if let Some((last_fail, count)) = failures.get(tool_name) {
            if *count >= self.fail_threshold {
                let elapsed = last_fail.elapsed().as_secs();
                if elapsed < self.block_secs {
                    return Some(self.block_secs - elapsed);
                }
                failures.remove(tool_name); // cooldown expired
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

impl Default for ToolGuardrail {
    fn default() -> Self {
        Self::new(3, 30, 10)
    }
}

// ---- Dedup Cache ----

/// Deduplicates identical tool calls within a single turn.
pub struct DedupCache {
    cache: Mutex<HashMap<String, String>>, // "tool_name:args_hash" → result
}

impl DedupCache {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Check if this call was already executed. Returns Some(cached_result) if yes.
    pub fn check(&self, tool_name: &str, args: &str) -> Option<String> {
        let key = format!("{}:{}", tool_name, args);
        self.cache.lock().unwrap().get(&key).cloned()
    }

    /// Store a result for dedup.
    pub fn insert(&self, tool_name: &str, args: &str, result: String) {
        let key = format!("{}:{}", tool_name, args);
        self.cache.lock().unwrap().insert(key, result);
    }

    /// Clear cache (called at turn end).
    pub fn clear(&self) {
        self.cache.lock().unwrap().clear();
    }
}

impl Default for DedupCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Inline Tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::ToolRegistry;
    use crate::models::ToolSchema;

    fn make_registry() -> ToolRegistry {
        let reg = ToolRegistry::new();
        reg.register_tool(
            ToolSchema { name: "read_file".to_string(), description: "".to_string(), parameters: serde_json::json!({}) },
            Arc::new(|_, _| Ok("ok".to_string())),
            crate::models::ToolSource::Builtin,
        );
        reg.register_tool(
            ToolSchema { name: "write_file".to_string(), description: "".to_string(), parameters: serde_json::json!({}) },
            Arc::new(|_, _| Ok("ok".to_string())),
            crate::models::ToolSource::Builtin,
        );
        reg.register_tool(
            ToolSchema { name: "mcp_fs_read".to_string(), description: "".to_string(), parameters: serde_json::json!({}) },
            Arc::new(|_, _| Ok("ok".to_string())),
            crate::models::ToolSource::Mcp { server: "fs".to_string() },
        );
        reg
    }

    // --- JSON truncation ---

    #[test]
    fn test_truncated_json_braces() {
        assert!(is_truncated_json(r#"{"key": "value""#));
    }

    #[test]
    fn test_truncated_json_brackets() {
        assert!(is_truncated_json(r#"["a", "b""#));
    }

    #[test]
    fn test_complete_json() {
        assert!(!is_truncated_json(r#"{"key": "value"}"#));
        assert!(!is_truncated_json(r#"["a", "b"]"#));
    }

    #[test]
    fn test_empty_json() {
        assert!(!is_truncated_json("{}"));
        assert!(!is_truncated_json("[]"));
    }

    #[test]
    fn test_truncated_json_trailing_comma() {
        assert!(is_truncated_json(r#"{"a": 1,}"#));
    }

    // --- Fuzzy matching ---

    #[test]
    fn test_normalize_tool_name() {
        assert_eq!(normalize_tool_name("Read-File"), "read_file");
        assert_eq!(normalize_tool_name("MCP.fs.read"), "mcp_fs_read");
    }

    #[test]
    fn test_fuzzy_match_exact() {
        let reg = make_registry();
        assert_eq!(fuzzy_match_tool("read_file", &reg), Some("read_file".to_string()));
    }

    #[test]
    fn test_fuzzy_match_normalized() {
        let reg = make_registry();
        assert_eq!(fuzzy_match_tool("Read-File", &reg), Some("read_file".to_string()));
    }

    #[test]
    fn test_fuzzy_match_prefix() {
        let reg = make_registry();
        assert_eq!(fuzzy_match_tool("mcp_fs", &reg), Some("mcp_fs_read".to_string()));
    }

    #[test]
    fn test_fuzzy_match_no_match() {
        let reg = make_registry();
        assert!(fuzzy_match_tool("totally_bogus_tool", &reg).is_none());
    }

    // --- Circuit breaker ---

    #[test]
    fn test_circuit_breaker_normal() {
        let cb = ToolGuardrail::new(3, 30, 10);
        assert!(cb.should_block("read_file").is_none());
        cb.record_success("read_file");
        assert!(cb.should_block("read_file").is_none());
    }

    #[test]
    fn test_circuit_breaker_triggers() {
        let cb = ToolGuardrail::new(3, 30, 10);
        cb.record_failure("read_file");
        cb.record_failure("read_file");
        assert!(cb.should_block("read_file").is_none()); // only 2, threshold is 3
        cb.record_failure("read_file");
        assert!(cb.should_block("read_file").is_some()); // 3rd failure triggers
    }

    #[test]
    fn test_circuit_breaker_independent_tools() {
        let cb = ToolGuardrail::new(3, 30, 10);
        cb.record_failure("read_file");
        cb.record_failure("read_file");
        cb.record_failure("read_file");
        assert!(cb.should_block("read_file").is_some());
        assert!(cb.should_block("write_file").is_none()); // write_file unaffected
    }

    // --- Dedup ---

    #[test]
    fn test_dedup_new_call() {
        let cache = DedupCache::new();
        assert!(cache.check("read_file", "{}").is_none());
    }

    #[test]
    fn test_dedup_duplicate() {
        let cache = DedupCache::new();
        cache.insert("read_file", "{}", "result1");
        assert_eq!(cache.check("read_file", "{}"), Some("result1".to_string()));
    }

    #[test]
    fn test_dedup_different_args() {
        let cache = DedupCache::new();
        cache.insert("read_file", r#"{"path": "a"}"#, "result_a");
        assert!(cache.check("read_file", r#"{"path": "b"}"#).is_none());
    }

    #[test]
    fn test_dedup_clear() {
        let cache = DedupCache::new();
        cache.insert("read_file", "{}", "result");
        cache.clear();
        assert!(cache.check("read_file", "{}").is_none());
    }
}
```

**Step 2: Add `all_tool_names` to ToolRegistry**

Edit `src/tool_registry.rs`:

```rust
    /// Return all registered tool names.
    pub fn all_tool_names(&self) -> Vec<String> {
        self.tools.lock().unwrap().keys().cloned().collect()
    }
```

**Step 3: Run tests**

Run: `cargo test guardrails`
Expected: All 9 tests pass

**Step 4: Commit**

```bash
git add src/guardrails.rs src/tool_registry.rs
git commit -m "feat: add tool call guardrails (fuzzy match, truncation, circuit breaker, dedup)"
```

---

### Task 8: Integrate guardrails into agent.rs

**Files:**
- Modify: `src/agent.rs`

**Step 1: Add guardrail and dedup fields to Agent struct**

```rust
pub guardrail: ToolGuardrail,
pub dedup_cache: DedupCache,
```

In `Agent::new()`:
```rust
guardrail: ToolGuardrail::default(),
dedup_cache: DedupCache::default(),
```

**Step 2: Add imports**

```rust
use crate::guardrails::{fuzzy_match_tool, is_truncated_json, ToolGuardrail, DedupCache};
```

**Step 3: Replace the tool processing loop**

Replace lines 212-289 (the entire tool call handling block) with:

```rust
            // Handle tool calls
            if let Some(tool_calls) = &assistant_msg.tool_calls {
                if !tool_calls.is_empty() {
                    log::info!("Processing {} tool call(s)...", tool_calls.len());

                    // Validate tool names
                    let mut valid_tool_calls = vec![];
                    let mut invalid_tools = vec![];

                    for tc in tool_calls {
                        if self.registry.has_tool(&tc.function.name) {
                            valid_tool_calls.push(tc.clone());
                        } else {
                            invalid_tools.push(tc.function.name.clone());
                        }
                    }

                    // Add assistant message with tool calls
                    let mut assistant_for_history = assistant_msg.clone();
                    assistant_for_history.content = assistant_for_history.content.filter(|c| !c.is_empty());
                    messages.push(assistant_for_history);

                    // Report invalid tools
                    if !invalid_tools.is_empty() {
                        log::warn!("Invalid tool calls: {:?}", invalid_tools);
                        for tc in tool_calls {
                            if invalid_tools.contains(&tc.function.name) {
                                messages.push(Message::tool(
                                    &tc.id,
                                    &tc.function.name,
                                    format!("Tool '{}' does not exist. Available: check skills_list or use built-in tools.", tc.function.name),
                                ));
                            }
                        }
                    }

                    // Execute valid tool calls with guardrails
                    for tc in &valid_tool_calls {
                        // Layer 1: Fuzzy tool name repair
                        let resolved_name = fuzzy_match_tool(&tc.function.name, &self.registry)
                            .unwrap_or_else(|| tc.function.name.clone());

                        // Layer 2: JSON truncation detection
                        if is_truncated_json(&tc.function.arguments) {
                            messages.push(Message::tool(
                                &tc.id,
                                &resolved_name,
                                "Error: tool arguments appear truncated (unclosed JSON). Please retry with complete arguments.".to_string(),
                            ));
                            continue;
                        }

                        // Layer 3: Circuit breaker check
                        if let Some(remaining) = self.guardrail.should_block(&resolved_name) {
                            messages.push(Message::tool(
                                &tc.id,
                                &resolved_name,
                                format!("Tool '{}' is temporarily unavailable due to repeated failures. Retry in {}s.", resolved_name, remaining),
                            ));
                            continue;
                        }

                        // Layer 4: Dedup check
                        let dedup_key = format!("{}:{}", resolved_name, tc.function.arguments);
                        if let Some(cached) = self.dedup_cache.check(&resolved_name, &tc.function.arguments) {
                            messages.push(Message::tool(&tc.id, &resolved_name, cached));
                            continue;
                        }

                        // Execute tool
                        let args: serde_json::Value = if tc.function.arguments.trim().is_empty() {
                            serde_json::json!({})
                        } else {
                            match serde_json::from_str(&tc.function.arguments) {
                                Ok(v) => v,
                                Err(_) => {
                                    messages.push(Message::tool(
                                        &tc.id,
                                        &resolved_name,
                                        format!("Error: invalid JSON arguments. Raw: {}", tc.function.arguments),
                                    ));
                                    continue;
                                }
                            }
                        };

                        self.emit(Event::ToolCall {
                            name: resolved_name.clone(),
                            args: args.clone(),
                        });
                        let timer = Timer::start();
                        let result = self.execute_tool_call_with_name(&resolved_name, &tc.function.id, &args);
                        let duration = timer.elapsed();
                        match result {
                            Ok(content) => {
                                self.emit(Event::ToolResult {
                                    name: resolved_name.clone(),
                                    success: true,
                                    duration,
                                    output_len: content.len(),
                                });
                                self.guardrail.record_success(&resolved_name);
                                self.dedup_cache.insert(&resolved_name, &tc.function.arguments, content.clone());
                                messages.push(Message::tool(&tc.id, &resolved_name, content));
                            }
                            Err(e) => {
                                let err_str = format!("{{\"error\": \"{}\"}}", e);
                                self.emit(Event::ToolResult {
                                    name: resolved_name.clone(),
                                    success: false,
                                    duration,
                                    output_len: err_str.len(),
                                });
                                self.guardrail.record_failure(&resolved_name);
                                messages.push(Message::tool(&tc.id, &resolved_name, err_str));
                            }
                        }
                    }

                    continue; // Loop back for next iteration
                }
            }
```

**Step 4: Add execute_tool_call_with_name helper**

Add alongside the existing `execute_tool_call`:

```rust
    fn execute_tool_call_with_name(&self, name: &str, id: &str, args: &serde_json::Value) -> Result<String> {
        log::info!("Executing tool: {} with args: {}", name, args);
        self.registry.dispatch(name, args)
    }
```

Wait, the existing `execute_tool_call` takes `&ToolCall` which has `tc.function.name`. We can either:
- Keep the existing method and add a new one that takes (name, id, args)
- Or refactor the existing one

Let's add a simpler helper that extracts name from ToolCall:

```rust
    fn execute_tool_call(&self, tc: &ToolCall) -> Result<String> {
        let args: serde_json::Value = if tc.function.arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&tc.function.arguments)?
        };
        log::info!("Executing tool: {} with args: {}", tc.function.name, args);
        self.registry.dispatch(&tc.function.name, &args)
    }
```

We need to keep this for backward compat but the new guardrail path doesn't use it. Actually, let's just keep the original method and add the guardrail path inline. The guardrail path already parses args itself, so no duplication.

Let me reconsider — the original `execute_tool_call` is fine. The guardrail code path parses args directly. No need for a new helper.

Actually, looking more carefully at the original code, `execute_tool_call` parses args again inside (line 341-345). The guardrail code path also parses args (line ~15 above). Let's remove the guardrail path's redundant parsing and call `execute_tool_call` instead, but we need to handle the `Result` the same way.

Hmm, actually the cleanest is to have the guardrail path call a dispatcher that takes (name, args):

```rust
    fn dispatch_tool(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        self.registry.dispatch(name, args)
    }
```

But this is trivial. Let's just keep it inline — the guardrail path already handles the Result, and calling `registry.dispatch` directly is one line. No need to over-abstract.

**Step 5: Add clear_dedup call at turn end**

In the turn-end section (around line 307, after history trimming), add:

```rust
self.guardrail = ToolGuardrail::default();  // Reset guardrail each turn
self.dedup_cache.clear();
```

Wait, we can't replace `self.guardrail` since it's not `mut self`. The Agent methods that need mutability... Let me check. `run_conversation` takes `&mut self`, so we can mutate fields.

Actually, simpler — just clear the failures and cache:

```rust
self.guardrail.failures.lock().unwrap().clear();
self.dedup_cache.clear();
```

But `failures` is private. Let me add a `reset` method to ToolGuardrail:

```rust
impl ToolGuardrail {
    pub fn reset(&self) {
        self.failures.lock().unwrap().clear();
    }
}
```

And call it at turn end.

**Step 6: Verify compilation**

Run: `cargo build`
Expected: Compiles

**Step 7: Run all tests**

Run: `cargo test`
Expected: All tests pass

**Step 8: Commit**

```bash
git add src/agent.rs src/guardrails.rs src/tool_registry.rs
git commit -m "feat: integrate tool call guardrails into agent loop"
```

---

### Task 9: End-to-end verification

**Step 1: Full test suite**

Run: `cargo test`
Expected: All tests pass, no warnings

**Step 2: Build check**

Run: `cargo build`
Expected: Clean build

**Step 3: Quick manual smoke test**

Run: `cargo run -- --help` or the project's normal startup
Expected: Agent starts without panics

**Step 4: Commit**

```bash
git commit -m "test: verify all three phases compile and pass"
```

---

### Summary of Changes

| File | Action | Description |
|------|--------|-------------|
| `src/ring_buffer.rs` | **New** | Generic RingBuffer<T> utility |
| `src/memory.rs` | **Modify** | Add Turn, MemoryFact, FactSource, WorkingMemoryProvider |
| `src/models.rs` | **Modify** | Add ReasoningDetail, extend ResponseMessage |
| `src/llm.rs` | **Modify** | Add extract_reasoning() 4-layer chain |
| `src/guardrails.rs` | **New** | ToolGuardrail, DedupCache, fuzzy_match_tool, is_truncated_json |
| `src/tool_registry.rs` | **Modify** | Add all_tool_names() |
| `src/agent.rs` | **Modify** | Wire in reasoning callback + guardrail pipeline |
| `src/main.rs` | **Modify** | Register WorkingMemoryProvider, add mod ring_buffer |

**Total: 2 new files, 5 modified files, ~350 lines of new code, 0 new dependencies.**
