//! Multi-layer memory system inspired by Hermes Agent.
//!
//! Four layers:
//! - Working Memory: Current conversation context (in-memory)
//! - Episodic Memory: Conversation history stored in SQLite
//! - Semantic Memory: Keyword/searchable long-term facts
//! - Procedural Memory: Skill usage patterns and tool preferences

use anyhow::{Context, Result};
use crate::file_memory::{FileMemoryStore, MemoryTarget};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// MemoryProvider trait — inspired by Hermes' MemoryProvider ABC
pub trait MemoryProvider: Send + Sync {
    fn name(&self) -> &str;
    fn prefetch(&self, query: &str, session_id: &str) -> Result<String>;
    fn sync_turn(&self, user: &str, assistant: &str, session_id: &str) -> Result<()>;
    fn on_turn_start(&self, _turn_number: usize, _message: &str) {}
    fn on_session_end(&self, _session_id: &str) -> Result<()> {
        Ok(())
    }
}

/// MemoryManager orchestrates multiple memory providers.
pub struct MemoryManager {
    providers: Vec<Arc<dyn MemoryProvider>>,
    file_memory: Option<Arc<FileMemoryStore>>,
}

impl MemoryManager {
    pub fn new() -> Self {
        Self {
            providers: vec![],
            file_memory: None,
        }
    }

    pub fn with_file_memory(mut self, store: Arc<FileMemoryStore>) -> Self {
        self.file_memory = Some(store);
        self
    }

    pub fn add_provider(&mut self, provider: Arc<dyn MemoryProvider>) {
        log::info!("Registering memory provider: {}", provider.name());
        self.providers.push(provider);
    }

    pub fn prefetch_all(&self, query: &str, session_id: &str) -> String {
        let mut parts = vec![];

        // File memory snapshot
        if let Some(ref fm) = self.file_memory {
            let mem_snapshot = fm.snapshot(MemoryTarget::Memory).unwrap_or_default();
            let user_snapshot = fm.snapshot(MemoryTarget::User).unwrap_or_default();

            if !mem_snapshot.is_empty() {
                parts.push(format!("## Agent Memory\n{}", mem_snapshot));
            }
            if !user_snapshot.is_empty() {
                parts.push(format!("## User Profile\n{}", user_snapshot));
            }
        }

        // Existing provider loop...
        for provider in &self.providers {
            match provider.prefetch(query, session_id) {
                Ok(ctx) if !ctx.trim().is_empty() => {
                    parts.push(format!("[{}]\n{}", provider.name(), ctx));
                }
                Ok(_) => {}
                Err(e) => log::debug!("Provider {} prefetch failed: {}", provider.name(), e),
            }
        }
        parts.join("\n\n")
    }

    pub fn sync_all(&self, user: &str, assistant: &str, session_id: &str) {
        for provider in &self.providers {
            if let Err(e) = provider.sync_turn(user, assistant, session_id) {
                log::warn!("Provider {} sync failed: {}", provider.name(), e);
            }
        }
    }

    pub fn on_turn_start(&self, turn_number: usize, message: &str) {
        for provider in &self.providers {
            provider.on_turn_start(turn_number, message);
        }
    }

    pub fn on_session_end(&self, session_id: &str) {
        for provider in &self.providers {
            if let Err(e) = provider.on_session_end(session_id) {
                log::warn!("Provider {} session end failed: {}", provider.name(), e);
            }
        }
    }
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a fenced memory context block (Hermes-style)
pub fn build_memory_context_block(raw_context: &str) -> String {
    if raw_context.trim().is_empty() {
        return String::new();
    }
    format!(
        "<memory-context>\n\
         [System note: The following is recalled memory context, \
         NOT new user input. Treat as authoritative reference data.]\n\n\
         {}\n\
         </memory-context>",
        raw_context
    )
}

// ---------------------------------------------------------------------------
// SQLite-backed multi-layer memory
// ---------------------------------------------------------------------------

pub struct SqliteMemory {
    conn: Mutex<Connection>,
    session_id: Mutex<String>,
}

impl SqliteMemory {
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open memory DB: {}", db_path.display()))?;
        
        let me = Self {
            conn: Mutex::new(conn),
            session_id: Mutex::new(String::new()),
        };
        me.init_tables()?;
        Ok(me)
    }

    fn init_tables(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "BEGIN;
            CREATE TABLE IF NOT EXISTS turns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                turn_number INTEGER NOT NULL,
                user_message TEXT,
                assistant_message TEXT,
                created_at TEXT NOT NULL,
                token_estimate INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_turns_session ON turns(session_id);
            
            CREATE TABLE IF NOT EXISTS semantic_memory (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key TEXT UNIQUE NOT NULL,
                value TEXT NOT NULL,
                category TEXT,
                importance REAL DEFAULT 1.0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                access_count INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_semantic_key ON semantic_memory(key);
            CREATE INDEX IF NOT EXISTS idx_semantic_category ON semantic_memory(category);
            
            CREATE TABLE IF NOT EXISTS episodic_summaries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                turn_range TEXT,
                created_at TEXT NOT NULL
            );
            
            CREATE TABLE IF NOT EXISTS procedural_memory (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                pattern_type TEXT NOT NULL,
                pattern_data TEXT NOT NULL,
                success_count INTEGER DEFAULT 0,
                fail_count INTEGER DEFAULT 0,
                created_at TEXT NOT NULL
            );
            
            CREATE TABLE IF NOT EXISTS user_profile (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                confidence REAL DEFAULT 0.5,
                updated_at TEXT NOT NULL
            );
            
            CREATE VIRTUAL TABLE IF NOT EXISTS semantic_memory_fts USING fts5(key, value, category);
            COMMIT;"
        )?;
        Ok(())
    }

    pub fn set_session_id(&self, session_id: &str) {
        *self.session_id.lock().unwrap() = session_id.to_string();
    }

    // --- Episodic ---

    pub fn save_turn(&self, turn_number: usize, user: &str, assistant: &str, tokens: i32) -> Result<()> {
        let session = self.session_id.lock().unwrap().clone();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO turns (session_id, turn_number, user_message, assistant_message, created_at, token_estimate)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session, turn_number as i64, user, assistant, Utc::now().to_rfc3339(), tokens],
        )?;
        Ok(())
    }

    pub fn get_recent_turns(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let session = self.session_id.lock().unwrap().clone();
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_message, assistant_message FROM turns
             WHERE session_id = ?1 ORDER BY turn_number DESC LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![session, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut results = vec![];
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    // --- Semantic ---

    pub fn remember(&self, key: &str, value: &str, category: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let cat = category.unwrap_or("general");
        conn.execute(
            "INSERT INTO semantic_memory (key, value, category, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value,
                 updated_at = excluded.updated_at,
                 importance = importance + 0.1",
            params![key, value, cat, now, now],
        )?;
        
        // Sync FTS5 index
        let id: i64 = conn.query_row(
            "SELECT id FROM semantic_memory WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO semantic_memory_fts(rowid, key, value, category)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(rowid) DO UPDATE SET
                 key = excluded.key,
                 value = excluded.value,
                 category = excluded.category",
            params![id, key, value, cat],
        )?;
        
        Ok(())
    }

    pub fn recall(&self, query: &str, top_k: usize) -> Result<Vec<(String, String, f64)>> {
        let conn = self.conn.lock().unwrap();
        // Simple keyword matching + importance ranking
        let keywords: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        
        let mut stmt = conn.prepare(
            "SELECT key, value, importance FROM semantic_memory ORDER BY importance DESC, updated_at DESC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        
        let mut scored = vec![];
        for row in rows {
            let (key, value, importance) = row?;
            let text = format!("{} {}", key, value).to_lowercase();
            let mut score = importance;
            for kw in &keywords {
                if text.contains(kw) {
                    score += 2.0;
                }
            }
            if score > importance {
                scored.push((key, value, score));
            }
        }
        scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        scored.truncate(top_k);
        Ok(scored)
    }

    /// Hybrid search: FTS5 BM25 ranking + importance + recency + access_count
    pub fn recall_hybrid(&self, query: &str, top_k: usize) -> Result<Vec<(String, String, f64)>> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now();
        
        // FTS5 search with BM25 ranking (rank is higher = more relevant)
        let mut stmt = conn.prepare(
            "SELECT s.id, s.key, s.value, s.importance, s.access_count, s.updated_at,
                    rank
             FROM semantic_memory_fts f
             JOIN semantic_memory s ON f.rowid = s.id
             WHERE f.semantic_memory_fts MATCH ?1
             ORDER BY rank DESC
             LIMIT ?2"
        )?;
        
        let rows = stmt.query_map(params![query, top_k as i64], |row| {
            Ok((
                row.get::<_, String>(1)?,  // key
                row.get::<_, String>(2)?,  // value
                row.get::<_, f64>(3)?,     // importance
                row.get::<_, i64>(4)?,     // access_count
                row.get::<_, String>(5)?,  // updated_at
                row.get::<_, f64>(6)?,     // rank (BM25-based)
            ))
        })?;
        
        let mut scored = vec![];
        for row in rows {
            let (key, value, importance, access_count, updated_at_str, rank) = row?;
            
            // Parse updated_at for recency boost
            let recency_boost = match chrono::DateTime::parse_from_rfc3339(&updated_at_str) {
                Ok(updated) => {
                    let days = (now - updated.with_timezone(&Utc)).num_days().max(0) as f64;
                    1.0 / (1.0 + days)
                }
                Err(_) => 0.5,
            };
            
            let access_boost = (access_count as f64 / 10.0).min(1.0);
            
            // Normalize rank (BM25 can be negative; shift to positive)
            let rank_score = (rank + 10.0).max(0.0) / 20.0; // rough normalization to 0..1
            
            let score = rank_score * 0.5
                      + importance * 0.2
                      + recency_boost * 0.2
                      + access_boost * 0.1;
            
            scored.push((key, value, score));
        }
        
        scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        scored.truncate(top_k);
        Ok(scored)
    }

    // --- User Profile ---

    pub fn set_profile(&self, key: &str, value: &str, confidence: f64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO user_profile (key, value, confidence, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value,
                 confidence = excluded.confidence,
                 updated_at = excluded.updated_at",
            params![key, value, confidence, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_profile(&self, key: &str) -> Result<Option<(String, f64)>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT value, confidence FROM user_profile WHERE key = ?1",
            params![key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
        ).optional()?;
        Ok(result)
    }

    pub fn get_all_profile(&self) -> Result<HashMap<String, (String, f64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT key, value, confidence FROM user_profile")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (k, v, c) = row?;
            map.insert(k, (v, c));
        }
        Ok(map)
    }

    pub fn cleanup_old_memories(&self, max_age_days: i32, min_importance: f64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(max_age_days as i64);
        let cutoff_str = cutoff.to_rfc3339();
        
        // Delete old low-importance memories from both tables
        conn.execute(
            "DELETE FROM semantic_memory
             WHERE updated_at < ?1 AND importance < ?2",
            params![cutoff_str, min_importance],
        )?;
        
        let deleted = conn.changes() as usize;
        
        // Also clean up orphaned FTS5 entries
        conn.execute(
            "DELETE FROM semantic_memory_fts
             WHERE rowid NOT IN (SELECT id FROM semantic_memory)",
            [],
        )?;
        
        Ok(deleted)
    }

    pub fn get_profile_snapshot(&self) -> Result<HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT key, value FROM user_profile")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, v);
        }
        Ok(map)
    }

    // --- Procedural ---

    pub fn record_tool_usage(&self, tool_name: &str, success: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let success_inc = if success { 1 } else { 0 };
        let fail_inc = if success { 0 } else { 1 };
        conn.execute(
            "INSERT INTO procedural_memory (pattern_type, pattern_data, success_count, fail_count, created_at)
             VALUES ('tool_usage', ?1, ?2, ?3, ?4)
             ON CONFLICT DO NOTHING",
            params![tool_name, success_inc, fail_inc, now],
        ).ok();
        // Update existing
        conn.execute(
            "UPDATE procedural_memory SET
                 success_count = success_count + ?2,
                 fail_count = fail_count + ?3
             WHERE pattern_type = 'tool_usage' AND pattern_data = ?1",
            params![tool_name, success_inc, fail_inc],
        )?;
        Ok(())
    }

    // --- Summarization ---

    pub fn save_episodic_summary(&self, summary: &str, turn_range: &str) -> Result<()> {
        let session = self.session_id.lock().unwrap().clone();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO episodic_summaries (session_id, summary, turn_range, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session, summary, turn_range, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_episodic_summaries(&self, limit: usize) -> Result<Vec<String>> {
        let session = self.session_id.lock().unwrap().clone();
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT summary FROM episodic_summaries
             WHERE session_id = ?1 ORDER BY created_at DESC LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![session, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }
}

/// Built-in memory provider using SQLite
pub struct BuiltinMemoryProvider {
    db: Arc<SqliteMemory>,
    turn_count: Mutex<usize>,
    top_k: usize,
    summary_threshold: usize,
    hybrid_search: bool,
}

impl BuiltinMemoryProvider {
    pub fn new(db: Arc<SqliteMemory>, top_k: usize, summary_threshold: usize, hybrid_search: bool) -> Self {
        Self {
            db,
            turn_count: Mutex::new(0),
            top_k,
            summary_threshold,
            hybrid_search,
        }
    }
}

impl MemoryProvider for BuiltinMemoryProvider {
    fn name(&self) -> &str {
        "builtin"
    }

    fn prefetch(&self, query: &str, _session_id: &str) -> Result<String> {
        let mut parts = vec![];
        
        // 1. User profile
        if let Ok(profile) = self.db.get_all_profile() {
            if !profile.is_empty() {
                let mut profile_lines = vec!["## User Profile".to_string()];
                for (k, (v, c)) in profile {
                    profile_lines.push(format!("- {}: {} (confidence: {:.0}%)", k, v, c * 100.0));
                }
                parts.push(profile_lines.join("\n"));
            }
        }
        
        // 2. Semantic memory recall (hybrid or keyword)
        let recall_result = if self.hybrid_search {
            self.db.recall_hybrid(query, self.top_k)
        } else {
            self.db.recall(query, self.top_k)
        };
        if let Ok(recalls) = recall_result {
            if !recalls.is_empty() {
                let mut mem_lines = vec!["## Relevant Memories".to_string()];
                for (key, value, score) in recalls {
                    mem_lines.push(format!("- {}: {} (score: {:.2})", key, value, score));
                }
                parts.push(mem_lines.join("\n"));
            }
        }
        
        // 3. Recent episodic summaries
        if let Ok(summaries) = self.db.get_episodic_summaries(3) {
            if !summaries.is_empty() {
                let mut sum_lines = vec!["## Past Conversation Summaries".to_string()];
                for s in summaries {
                    sum_lines.push(format!("- {}", s));
                }
                parts.push(sum_lines.join("\n"));
            }
        }
        
        Ok(parts.join("\n\n"))
    }

    fn sync_turn(&self, user: &str, assistant: &str, session_id: &str) -> Result<()> {
        self.db.set_session_id(session_id);
        let mut count = self.turn_count.lock().unwrap();
        *count += 1;
        self.db.save_turn(*count, user, assistant, 0)?;
        
        // Auto-summarize when threshold reached
        if *count % self.summary_threshold == 0 {
            if let Ok(turns) = self.db.get_recent_turns(self.summary_threshold) {
                let summary = format_summary(&turns);
                let range = format!("turns {}-{}", *count - self.summary_threshold + 1, *count);
                self.db.save_episodic_summary(&summary, &range).ok();
            }
        }
        
        // Extract potential facts for semantic memory
        extract_facts_to_semantic(self.db.clone(), user, assistant).ok();
        
        Ok(())
    }

    fn on_turn_start(&self, turn_number: usize, _message: &str) {
        let mut count = self.turn_count.lock().unwrap();
        *count = turn_number;
    }
}

fn format_summary(turns: &[(String, String)]) -> String {
    let topics: Vec<String> = turns
        .iter()
        .map(|(u, _a)| {
            let words: Vec<&str> = u.split_whitespace().take(5).collect();
            words.join(" ")
        })
        .collect();
    format!("Discussed: {}", topics.join("; "))
}

fn extract_facts_to_semantic(db: Arc<SqliteMemory>, user: &str, assistant: &str) -> Result<()> {
    // Simple heuristic extraction
    let combined = format!("{} {}", user, assistant);
    
    // Extract "I like..." patterns
    if let Some(idx) = combined.to_lowercase().find("i like ") {
        let start = idx + 7;
        let rest = &combined[start..];
        let end = rest.find(|c: char| c == '.' || c == '!' || c == '?').unwrap_or(rest.len());
        let fact = rest[..end].trim();
        if !fact.is_empty() && fact.len() < 200 {
            db.remember(&format!("preference:{}", &fact[..fact.len().min(40)]), fact, Some("preference"))?;
        }
    }
    
    // Extract "I prefer..." patterns
    if let Some(idx) = combined.to_lowercase().find("i prefer ") {
        let start = idx + 9;
        let rest = &combined[start..];
        let end = rest.find(|c: char| c == '.' || c == '!' || c == '?').unwrap_or(rest.len());
        let fact = rest[..end].trim();
        if !fact.is_empty() && fact.len() < 200 {
            db.remember(&format!("preference:{}", &fact[..fact.len().min(40)]), fact, Some("preference"))?;
        }
    }
    
    Ok(())
}
