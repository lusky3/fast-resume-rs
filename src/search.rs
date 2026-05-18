/// SessionSearch: orchestrates incremental adapter scans and Tantivy indexing.
///
/// Ported from python/fast_resume/search.py.

use std::collections::HashMap;

use anyhow::Result;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::adapters::{AgentAdapter, IncrementalResult};
use crate::config;
use crate::index::TantivyIndex;
use crate::query::{parse_query, Filter};
use crate::session::Session;

// Import concrete adapters.
use crate::adapters::claude::ClaudeAdapter;
use crate::adapters::codex::CodexAdapter;
use crate::adapters::copilot::CopilotAdapter;
use crate::adapters::copilot_vscode::CopilotVSCodeAdapter;
use crate::adapters::crush::CrushAdapter;
use crate::adapters::gemini::GeminiAdapter;
use crate::adapters::kiro::KiroAdapter;
use crate::adapters::opencode::OpenCodeAdapter;
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
            Box::new(CopilotVSCodeAdapter::new()),
            Box::new(CrushAdapter::new()),
            Box::new(GeminiAdapter::new()),
            Box::new(KiroAdapter::new()),
            Box::new(OpenCodeAdapter::new()),
            Box::new(VibeAdapter::new()),
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
    ///
    /// The query string is parsed for keyword DSL tokens (`agent:`, `dir:`, `date:`)
    /// before being passed to the Tantivy index.  Any parsed filter is applied as a
    /// Tantivy filter query; the remaining free-text goes through the hybrid BM25 +
    /// fuzzy ranker.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Session>> {
        self.index.ensure_index()?;

        // Parse the query DSL.
        let parsed = parse_query(query);

        // Apply agent filter from filter bar if there is no agent: keyword.
        let hits = self.index.search_with_filters(
            &parsed.text,
            parsed.agent.as_ref(),
            parsed.directory.as_ref(),
            parsed.date.as_ref(),
            limit,
        )?;

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

    /// Search with an explicit agent filter (e.g. from the TUI filter bar).
    pub fn search_with_agent_filter(
        &self,
        query: &str,
        agent_filter: Option<&Filter>,
        limit: usize,
    ) -> Result<Vec<Session>> {
        self.index.ensure_index()?;

        let parsed = parse_query(query);

        // The caller's agent_filter overrides any agent: keyword in the query
        // when both are present.
        let effective_agent = agent_filter.or(parsed.agent.as_ref());

        let hits = self.index.search_with_filters(
            &parsed.text,
            effective_agent,
            parsed.directory.as_ref(),
            parsed.date.as_ref(),
            limit,
        )?;

        if hits.is_empty() {
            return Ok(Vec::new());
        }

        let all_sessions = self.index.get_all_sessions()?;
        let by_id: HashMap<String, Session> =
            all_sessions.into_iter().map(|s| (s.id.clone(), s)).collect();

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

    /// Look up the adapter for a given agent name.
    pub fn get_adapter_for_agent(&self, agent: &str) -> Option<&dyn AgentAdapter> {
        self.adapters
            .iter()
            .find(|a| a.name() == agent)
            .map(|a| a.as_ref())
    }

    /// Return raw stats for all adapters (for `fr --stats`).
    pub fn get_raw_stats(&self) -> Vec<crate::session::RawAdapterStats> {
        self.adapters.iter().map(|a| a.get_raw_stats()).collect()
    }
}

impl Default for SessionSearch {
    fn default() -> Self {
        Self::new()
    }
}
