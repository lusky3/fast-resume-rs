/// SessionSearch: orchestrates incremental adapter scans and Tantivy indexing.
///
/// Ported from python/fast_resume/search.py.

use std::collections::HashMap;

use anyhow::Result;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::adapters::{AgentAdapter, IncrementalResult};
use crate::config;
use crate::index::TantivyIndex;
use crate::session::Session;

// Import concrete adapters.
use crate::adapters::claude::ClaudeAdapter;
use crate::adapters::codex::CodexAdapter;
use crate::adapters::copilot::CopilotAdapter;
use crate::adapters::kiro::KiroAdapter;
use crate::adapters::vibe::VibeAdapter;

pub struct SessionSearch {
    pub adapters: Vec<Box<dyn AgentAdapter>>,
    index: TantivyIndex,
}

impl SessionSearch {
    /// Construct with all available adapters and the default index path.
    pub fn new() -> Self {
        let adapters: Vec<Box<dyn AgentAdapter>> = vec![
            Box::new(ClaudeAdapter::new()),
            Box::new(CodexAdapter::new()),
            Box::new(CopilotAdapter::new()),
            Box::new(VibeAdapter::new()),
            Box::new(KiroAdapter::new()),
        ];
        Self {
            adapters,
            index: TantivyIndex::new(config::index_dir()),
        }
    }

    /// Construct with a custom set of adapters and index path (for tests).
    pub fn with_adapters_and_index(
        adapters: Vec<Box<dyn AgentAdapter>>,
        index: TantivyIndex,
    ) -> Self {
        Self { adapters, index }
    }

    /// Scan all adapters, apply incremental updates to the index, and return
    /// all indexed sessions.
    ///
    /// Strategy (Phase 2):
    ///   1. Load known sessions from the index.
    ///   2. Dispatch adapter scans in parallel with rayon.
    ///   3. Collect all IncrementalResult values (sessions are Send).
    ///   4. Write updates and deletes sequentially in the calling thread.
    ///   5. Return the full session set from the index.
    pub fn get_all_sessions(&self) -> Result<Vec<Session>> {
        self.index.ensure_index()?;

        let known = self.index.get_known_sessions()?;

        // Parallel scan across adapters.
        let delta_results: Vec<IncrementalResult> = self
            .adapters
            .par_iter()
            .map(|adapter| adapter.find_sessions_incremental(&known, None, None))
            .collect();

        // Merge results sequentially, then write to index.
        let mut all_new_or_modified: Vec<Session> = Vec::new();
        let mut all_deleted_ids: Vec<String> = Vec::new();

        for delta in delta_results {
            all_new_or_modified.extend(delta.new_or_modified);
            all_deleted_ids.extend(delta.deleted_ids);
        }

        if !all_new_or_modified.is_empty() {
            self.index.update_sessions(&all_new_or_modified)?;
        }
        if !all_deleted_ids.is_empty() {
            self.index.delete_sessions(&all_deleted_ids)?;
        }

        self.index.get_all_sessions()
    }

    /// Search the index for `query`, look up full sessions by id, and return
    /// the matching sessions in score order.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Session>> {
        self.index.ensure_index()?;

        let hits = self.index.search(query, limit)?;
        if hits.is_empty() {
            return Ok(Vec::new());
        }

        // Build id→session map from the index for fast lookup.
        let all_sessions = self.index.get_all_sessions()?;
        let by_id: HashMap<String, Session> = all_sessions
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();

        let results = hits
            .into_iter()
            .filter_map(|(id, _score)| by_id.get(&id).cloned())
            .collect();
        Ok(results)
    }

    /// Returns the total number of indexed sessions.
    pub fn get_session_count(&self) -> Result<u64> {
        self.index.ensure_index()?;
        self.index.get_session_count()
    }

    /// Returns the number of adapters registered.
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }
}

impl Default for SessionSearch {
    fn default() -> Self {
        Self::new()
    }
}
