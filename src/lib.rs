//! Library facade for mini-agent.
//!
//! This file re-exports the public API so that integration tests in `tests/`
//! can import via `mini_agent::...`.

pub mod agent;
pub mod compression;
pub mod config;
pub mod file_memory;
pub mod heartbeat;
pub mod hooks;
pub mod identity;
pub mod llm;
pub mod memory;
pub mod mcp;
pub mod models;
pub mod observer;
pub mod session_search;
pub mod skill;
pub mod tool;
pub mod tool_registry;

// Re-exports for convenient integration-test access
pub use file_memory::{FileMemoryStore, MemoryTarget};
pub use session_search::SessionDB;
pub use compression::ContextCompressor;
pub use memory::{MemoryManager, SqliteMemory};
pub use models::Message;
