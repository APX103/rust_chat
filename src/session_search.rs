//! Session search — SQLite + FTS5 full-text search across all sessions.
//!
//! Stores every conversation turn and provides three search modes:
//! - Discovery: FTS5 query → matching sessions with snippets
//! - Scroll: session_id + message_id → paginated message window
//! - Browse: recent sessions list

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SessionSearchResult {
    pub session_id: String,
    pub title: Option<String>,
    pub started_at: String,
    pub message_count: i64,
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub role: String,
    pub content: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    pub started_at: String,
    pub message_count: i64,
    pub total_tokens: i64,
}

// ---------------------------------------------------------------------------
// SessionDB
// ---------------------------------------------------------------------------

pub struct SessionDB {
    conn: Mutex<Connection>,
}

impl SessionDB {
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open session DB: {}", db_path.display()))?;
        let me = Self {
            conn: Mutex::new(conn),
        };
        me.init_tables()?;
        Ok(me)
    }

    fn init_tables(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "BEGIN;
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                parent_session_id TEXT,
                message_count INTEGER DEFAULT 0,
                total_tokens INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_calls TEXT,
                tool_call_id TEXT,
                timestamp TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
            CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                content=sessions,
                tokenize='porter unicode61'
            );
            CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
                INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
            END;
            COMMIT;"
        )?;
        Ok(())
    }

    // --- Session CRUD ---

    pub fn upsert_session(&self, id: &str, title: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO sessions (id, title, started_at, message_count, total_tokens)
             VALUES (?1, ?2, ?3, 0, 0)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title",
            params![id, title, now],
        )?;
        Ok(())
    }

    pub fn end_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn update_session_meta(&self, id: &str, title: Option<&str>, tokens: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET title = COALESCE(?2, title), total_tokens = ?3 WHERE id = ?1",
            params![id, title, tokens],
        )?;
        Ok(())
    }

    // --- Messages ---

    pub fn store_message(
        &self,
        session_id: &str,
        role: &str,
        content: Option<&str>,
        tool_calls: Option<&str>,
        tool_call_id: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, role, content, tool_calls, tool_call_id, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn increment_message_count(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET message_count = message_count + 1 WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    // --- Search modes ---

    /// Discovery: FTS5 full-text search across all sessions.
    pub fn discover(&self, query: &str, limit: usize) -> Result<Vec<SessionSearchResult>> {
        let conn = self.conn.lock().unwrap();

        if query.is_empty() {
            // Fall back to recent sessions when no query
            let summaries = self.browse(limit)?;
            return Ok(summaries
                .into_iter()
                .map(|s| SessionSearchResult {
                    session_id: s.id,
                    title: s.title,
                    started_at: s.started_at,
                    message_count: s.message_count,
                    snippet: String::new(),
                })
                .collect());
        }

        let mut stmt = conn.prepare(
            "SELECT s.id, s.title, s.started_at, s.message_count,
                    snippet(messages_fts, 1, '<', '>', '...', 64) as snippet
             FROM messages_fts f
             JOIN sessions s ON f.rowid IN (
                 SELECT id FROM messages WHERE session_id = s.id
             )
             WHERE messages_fts MATCH ?1
             GROUP BY s.id
             ORDER BY s.started_at DESC
             LIMIT ?2"
        )?;

        let rows = stmt
            .query_map(params![query, limit as i64], |row| {
                Ok(SessionSearchResult {
                    session_id: row.get(0)?,
                    title: row.get(1)?,
                    started_at: row.get(2)?,
                    message_count: row.get(3)?,
                    snippet: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::Error::from(e))?;

        Ok(rows)
    }

    /// Scroll: paginated messages within a session, optionally centered on a message.
    pub fn scroll(&self, session_id: &str, around_id: Option<i64>, window: usize) -> Result<Vec<MessageRow>> {
        let conn = self.conn.lock().unwrap();

        let rows: Vec<MessageRow> = if let Some(anchor) = around_id {
            // Find the anchor's position, then paginate around it
            let anchor_ts: Option<String> = conn
                .query_row(
                    "SELECT timestamp FROM messages WHERE id = ?1 AND session_id = ?2",
                    params![anchor, session_id],
                    |row| row.get(0),
                )
                .optional()?;

            let anchor_ts = match anchor_ts {
                Some(ts) => ts,
                None => return Ok(vec![]),
            };

            let half = window / 2;
            let mut stmt = conn.prepare(
                "SELECT id, role, content, timestamp
                 FROM messages
                 WHERE session_id = ?1
                   AND timestamp <= ?2
                 ORDER BY timestamp DESC
                 LIMIT ?3"
            )?;
            let before: Vec<MessageRow> = stmt
                .query_map(params![session_id, anchor_ts, half], |row| {
                    Ok(MessageRow {
                        id: row.get(0)?,
                        role: row.get(1)?,
                        content: row.get(2)?,
                        timestamp: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            let after_count = window - before.len();
            let mut stmt2 = conn.prepare(
                "SELECT id, role, content, timestamp
                 FROM messages
                 WHERE session_id = ?1
                   AND timestamp > ?2
                 ORDER BY timestamp ASC
                 LIMIT ?3"
            )?;
            let after: Vec<MessageRow> = stmt2
                .query_map(params![session_id, anchor_ts, after_count], |row| {
                    Ok(MessageRow {
                        id: row.get(0)?,
                        role: row.get(1)?,
                        content: row.get(2)?,
                        timestamp: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            // Merge: before (reversed) + after
            let mut combined = before;
            combined.reverse();
            combined.extend(after);
            combined
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, role, content, timestamp
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY timestamp ASC
                 LIMIT ?2"
            )?;
            let result: Vec<MessageRow> = stmt
                .query_map(params![session_id, window as i64], |row| {
                    Ok(MessageRow {
                        id: row.get(0)?,
                        role: row.get(1)?,
                        content: row.get(2)?,
                        timestamp: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            result
        };

        Ok(rows)
    }

    /// Browse: recent sessions summary.
    pub fn browse(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, started_at, message_count, total_tokens
             FROM sessions
             ORDER BY started_at DESC
             LIMIT ?1"
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(SessionSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    started_at: row.get(2)?,
                    message_count: row.get(3)?,
                    total_tokens: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::Error::from(e))?;

        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_crud() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let db = SessionDB::new(&tmp.path().join("sessions.db"))?;
        db.upsert_session("s1", Some("Test Session"))?;
        db.store_message("s1", "user", Some("Hello"), None, None)?;
        db.store_message("s1", "assistant", Some("Hi there"), None, None)?;
        db.increment_message_count("s1")?;
        db.increment_message_count("s1")?;
        db.end_session("s1")?;
        let sessions = db.browse(10)?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "s1");
        assert_eq!(sessions[0].message_count, 2);
        let msgs = db.scroll("s1", None, 10)?;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        Ok(())
    }

    #[test]
    fn test_discover_empty_query() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let db = SessionDB::new(&tmp.path().join("sessions.db"))?;
        db.upsert_session("s1", Some("Test"))?;
        let results = db.discover("", 5)?;
        // Empty query falls back to browse, so we should see the session
        assert_eq!(results.len(), 1);
        Ok(())
    }

    #[test]
    fn test_scroll_with_anchor() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let db = SessionDB::new(&tmp.path().join("sessions.db"))?;
        db.upsert_session("s1", Some("Test"))?;
        let id1 = db.store_message("s1", "user", Some("msg1"), None, None)?;
        let _id2 = db.store_message("s1", "assistant", Some("msg2"), None, None)?;
        let _id3 = db.store_message("s1", "user", Some("msg3"), None, None)?;

        // Scroll around the second message with window=2
        let msgs = db.scroll("s1", Some(id1), 2)?;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, Some("msg1".to_string()));
        Ok(())
    }

    #[test]
    fn test_update_session_meta() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let db = SessionDB::new(&tmp.path().join("sessions.db"))?;
        db.upsert_session("s1", Some("Original"))?;
        db.update_session_meta("s1", Some("Updated"), 42)?;
        let sessions = db.browse(10)?;
        assert_eq!(sessions[0].title, Some("Updated".to_string()));
        assert_eq!(sessions[0].total_tokens, 42);
        Ok(())
    }
}
