/// Adapter trait and helpers for agent session adapters.
///
/// Mirrors python/fast_resume/adapters/base.py.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod copilot_vscode;
pub mod crush;
pub mod gemini;
pub mod kiro;
pub mod opencode;
pub mod vibe;

use std::collections::HashMap;
use std::path::PathBuf;

use crate::session::{ParseError, RawAdapterStats, Session};

/// 1 ms tolerance for mtime comparison due to floating-point precision.
pub const MTIME_TOLERANCE: f64 = 0.001;

/// Callback invoked when a parse error occurs.
pub type ErrorCb<'a> = &'a (dyn Fn(ParseError) + Send + Sync);

/// Callback invoked when a session is successfully parsed (for progressive indexing).
pub type SessionCb<'a> = &'a (dyn Fn(Session) + Send + Sync);

/// Result of an incremental scan.
pub struct IncrementalResult {
    pub new_or_modified: Vec<Session>,
    pub deleted_ids: Vec<String>,
}

/// Trait that every agent adapter must implement.
pub trait AgentAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn color(&self) -> &'static str;
    fn badge(&self) -> &'static str;

    /// Whether the adapter supports a yolo/skip-permissions flag.
    fn supports_yolo(&self) -> bool {
        false
    }

    /// Returns true if the agent's data directory exists.
    fn is_available(&self) -> bool;

    /// Find all sessions for this agent (full scan, no incremental).
    fn find_sessions(&self) -> Vec<Session>;

    /// Incremental scan: only re-parse files whose mtime exceeds the stored value.
    ///
    /// `known` maps session_id → (mtime, agent_name).
    fn find_sessions_incremental(
        &self,
        known: &HashMap<String, (f64, String)>,
        on_error: Option<ErrorCb<'_>>,
        on_session: Option<SessionCb<'_>>,
    ) -> IncrementalResult;

    /// Return the argv list to exec when resuming `session`.
    fn get_resume_command(&self, session: &Session, yolo: bool) -> Vec<String>;

    /// Return raw statistics about the adapter's data directory.
    fn get_raw_stats(&self) -> RawAdapterStats;
}

/// Template helper for file-based adapters.
///
/// Implements the same logic as `BaseSessionAdapter.find_sessions_incremental` in Python:
/// - Calls `scan()` to enumerate `(session_id, path, mtime)` tuples.
/// - For each file whose mtime exceeds `known[id] + MTIME_TOLERANCE`, calls `parse()`.
/// - Detects deleted sessions: those in `known` for this adapter but absent from scan.
///
/// Adapters call this from their `find_sessions_incremental` implementation.
pub fn file_based_incremental(
    name: &str,
    available: bool,
    known: &HashMap<String, (f64, String)>,
    scan: impl Fn() -> Vec<(String, PathBuf, f64)>,
    parse: impl Fn(&std::path::Path, Option<ErrorCb<'_>>) -> Option<Session>,
    on_error: Option<ErrorCb<'_>>,
    on_session: Option<SessionCb<'_>>,
) -> IncrementalResult {
    if !available {
        // All known sessions from this agent are considered deleted.
        let deleted_ids = known
            .iter()
            .filter(|(_, (_, agent))| agent == name)
            .map(|(id, _)| id.clone())
            .collect();
        return IncrementalResult {
            new_or_modified: vec![],
            deleted_ids,
        };
    }

    let current_files = scan();
    let current_ids: std::collections::HashSet<String> =
        current_files.iter().map(|(id, _, _)| id.clone()).collect();

    let mut new_or_modified = Vec::new();

    for (session_id, path, mtime) in &current_files {
        let needs_parse = match known.get(session_id) {
            None => true,
            Some((known_mtime, _)) => mtime > &(known_mtime + MTIME_TOLERANCE),
        };

        if needs_parse {
            if let Some(mut session) = parse(path, on_error) {
                session.mtime = *mtime;
                if let Some(cb) = on_session {
                    cb(session.clone());
                }
                new_or_modified.push(session);
            }
        }
    }

    // Sessions that existed before for this agent but are no longer on disk.
    let deleted_ids = known
        .iter()
        .filter(|(id, (_, agent))| agent == name && !current_ids.contains(*id))
        .map(|(id, _)| id.clone())
        .collect();

    IncrementalResult {
        new_or_modified,
        deleted_ids,
    }
}
