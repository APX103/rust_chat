//! Background heartbeat tasks — inspired by ZeroClaw's HEARTBEAT.md.
//!
//! Runs periodic maintenance tasks in a background thread:
//! - Auto-summarize conversations when threshold is reached
//! - Clean up old low-importance memories
//! - Log profile change summaries

use crate::memory::SqliteMemory;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct Heartbeat {
    handle: Option<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub enum HeartbeatTask {
    AutoSummarize,
    MemoryCleanup,
    ProfileReport,
}

impl Heartbeat {
    pub fn new() -> Self {
        Self {
            handle: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(
        &mut self,
        interval_secs: u64,
        tasks: Vec<String>,
        db: Arc<SqliteMemory>,
    ) {
        let shutdown = self.shutdown.clone();
        let parsed_tasks: Vec<HeartbeatTask> = tasks
            .into_iter()
            .filter_map(|t| match t.as_str() {
                "auto_summarize" => Some(HeartbeatTask::AutoSummarize),
                "memory_cleanup" => Some(HeartbeatTask::MemoryCleanup),
                "profile_report" => Some(HeartbeatTask::ProfileReport),
                other => {
                    log::warn!("Unknown heartbeat task: {}", other);
                    None
                }
            })
            .collect();

        if parsed_tasks.is_empty() {
            log::info!("Heartbeat started with no tasks.");
            return;
        }

        log::info!(
            "Starting heartbeat: interval={}s, tasks={:?}",
            interval_secs,
            parsed_tasks
        );

        self.handle = Some(thread::spawn(move || {
            let interval = Duration::from_secs(interval_secs);
            loop {
                thread::sleep(interval);
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                for task in &parsed_tasks {
                    if let Err(e) = run_task(task, &db) {
                        log::warn!("Heartbeat task {:?} failed: {}", task, e);
                    }
                }
            }
            log::info!("Heartbeat thread stopped.");
        }));
    }

    pub fn stop(&mut self) {
        if self.handle.is_some() {
            log::info!("Stopping heartbeat...");
            self.shutdown.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                // Give the thread a few seconds to exit gracefully
                let _ = handle.join();
            }
        }
    }
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self::new()
    }
}

fn run_task(task: &HeartbeatTask, db: &SqliteMemory) -> Result<()> {
    match task {
        HeartbeatTask::AutoSummarize => {
            // Check if there are enough recent turns to summarize
            // For now, just log that the task ran; summarization
            // is handled inline during sync_turn to avoid complexity
            log::debug!("[heartbeat] Auto-summarize check");
            Ok(())
        }
        HeartbeatTask::MemoryCleanup => {
            let deleted = db.cleanup_old_memories(30, 0.1)?;
            if deleted > 0 {
                log::info!("[heartbeat] Cleaned up {} old memories", deleted);
            } else {
                log::debug!("[heartbeat] No old memories to clean up");
            }
            Ok(())
        }
        HeartbeatTask::ProfileReport => {
            // Fetch current profile and log a summary
            match db.get_profile_snapshot() {
                Ok(profile) if !profile.is_empty() => {
                    let keys: Vec<String> = profile.keys().cloned().collect();
                    log::info!("[heartbeat] User profile keys: {:?}", keys);
                }
                _ => {
                    log::debug!("[heartbeat] No profile data yet");
                }
            }
            Ok(())
        }
    }
}
