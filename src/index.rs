/// Tantivy full-text search index for fast-resume sessions.
///
/// Ported from python/fast_resume/index.py.
/// Schema version bumped to 22 for the Rust port.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use std::ops::Bound;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tantivy::collector::{DocSetCollector, TopDocs};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, QueryParser, RangeQuery,
    RegexQuery, TermSetQuery,
};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, STORED, TEXT,
};
use tantivy::schema::Value as TantivyValue;
use tantivy::{Index, IndexWriter, TantivyDocument, Term};

use crate::config::SCHEMA_VERSION;
use crate::query::{DateFilter, DateOp, Filter};
use crate::session::Session;

const VERSION_FILE: &str = ".schema_version";
const WRITER_HEAP_BYTES: usize = 50_000_000; // 50 MB

/// Typed handles to each schema field.
struct SchemaFields {
    id: Field,
    title: Field,
    directory: Field,
    agent: Field,
    content: Field,
    timestamp: Field,
    message_count: Field,
    mtime: Field,
    yolo: Field,
}

/// Live state after the index has been opened or created.
struct TantivyState {
    index: Index,
    #[allow(dead_code)]
    schema: Schema,
    fields: SchemaFields,
}

/// Thread-safe wrapper around a lazily-initialised Tantivy index.
pub struct TantivyIndex {
    index_path: PathBuf,
    inner: Mutex<Option<TantivyState>>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Schema builder
// ──────────────────────────────────────────────────────────────────────────────

fn raw_text_options() -> TextOptions {
    TextOptions::default().set_stored().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("raw")
            .set_index_option(IndexRecordOption::Basic),
    )
}

fn build_schema() -> (Schema, SchemaFields) {
    let mut b = Schema::builder();

    // id: exact (raw tokenizer) for lookup, stored
    let id = b.add_text_field("id", raw_text_options());
    // title: full-text, stored
    let title = b.add_text_field("title", TEXT | STORED);
    // directory: raw for exact queries, stored
    let directory = b.add_text_field("directory", raw_text_options());
    // agent: raw (preserves hyphens like "copilot-cli"), stored
    let agent = b.add_text_field("agent", raw_text_options());
    // content: full-text, stored
    let content = b.add_text_field("content", TEXT | STORED);
    // timestamp: f64 seconds-since-epoch, indexed + fast for range queries
    let timestamp = b.add_f64_field(
        "timestamp",
        NumericOptions::default()
            .set_stored()
            .set_indexed()
            .set_fast(),
    );
    // message_count: u64, stored only
    let message_count = b.add_u64_field("message_count", STORED);
    // mtime: f64 seconds-since-epoch, stored only (used for incremental checks)
    let mtime = b.add_f64_field("mtime", NumericOptions::default().set_stored());
    // yolo: bool, stored only
    let yolo = b.add_bool_field("yolo", NumericOptions::default().set_stored());

    let schema = b.build();
    let fields = SchemaFields {
        id,
        title,
        directory,
        agent,
        content,
        timestamp,
        message_count,
        mtime,
        yolo,
    };
    (schema, fields)
}

// ──────────────────────────────────────────────────────────────────────────────
// Index creation / version management
// ──────────────────────────────────────────────────────────────────────────────

/// Open or create the Tantivy index at `path`, wiping if the schema version
/// does not match `SCHEMA_VERSION`.
fn open_or_create_index(path: &Path) -> Result<(Index, Schema, SchemaFields)> {
    let version_path = path.join(VERSION_FILE);

    let need_rebuild = if path.exists() {
        // version_path is always config::index_dir().join(".schema_version") —
        // a compile-time constant joined to a fixed cache directory; not user input.
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let stored = fs::read_to_string(&version_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        stored != Some(SCHEMA_VERSION)
    } else {
        true
    };

    if need_rebuild && path.exists() {
        fs::remove_dir_all(path).context("removing stale index directory")?;
    }
    fs::create_dir_all(path).context("creating index directory")?;

    let (schema, fields) = build_schema();
    let dir = tantivy::directory::MmapDirectory::open(path)
        .context("opening MmapDirectory for tantivy")?;
    let index = Index::open_or_create(dir, schema.clone())
        .context("opening or creating tantivy index")?;

    // Write version stamp after successful creation.
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    fs::write(&version_path, SCHEMA_VERSION.to_string())
        .context("writing schema version file")?;

    Ok((index, schema, fields))
}

// ──────────────────────────────────────────────────────────────────────────────
// Document ↔ Session conversion
// ──────────────────────────────────────────────────────────────────────────────

fn session_to_doc(s: &Session, fields: &SchemaFields) -> TantivyDocument {
    let ts_f64 = s.timestamp.as_second() as f64
        + (s.timestamp.subsec_nanosecond() as f64 / 1_000_000_000.0);

    tantivy::doc!(
        fields.id => s.id.as_str(),
        fields.title => s.title.as_str(),
        fields.directory => s.directory.as_str(),
        fields.agent => s.agent.as_str(),
        fields.content => s.content.as_str(),
        fields.timestamp => ts_f64,
        fields.message_count => s.message_count as u64,
        fields.mtime => s.mtime,
        fields.yolo => s.yolo
    )
}

fn doc_to_session(doc: &TantivyDocument, fields: &SchemaFields) -> Option<Session> {
    let id = doc.get_first(fields.id)?.as_str()?.to_string();
    let title = doc
        .get_first(fields.title)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let directory = doc
        .get_first(fields.directory)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let agent = doc
        .get_first(fields.agent)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content = doc
        .get_first(fields.content)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let timestamp_f64 = doc
        .get_first(fields.timestamp)
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let message_count = doc
        .get_first(fields.message_count)
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let mtime = doc
        .get_first(fields.mtime)
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let yolo = doc
        .get_first(fields.yolo)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let whole = timestamp_f64 as i64;
    let nanos = ((timestamp_f64 - whole as f64) * 1_000_000_000.0) as i32;
    let timestamp =
        jiff::Timestamp::new(whole, nanos).unwrap_or(jiff::Timestamp::UNIX_EPOCH);

    Some(Session {
        id,
        agent,
        title,
        directory,
        timestamp,
        content,
        message_count,
        mtime,
        yolo,
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Hybrid query builder
// ──────────────────────────────────────────────────────────────────────────────

/// Build a hybrid BM25 + fuzzy query over `title` and `content`.
///
/// - Exact BM25 via `QueryParser` (boosted 5×) so precise matches rank first.
/// - Fuzzy per-term queries (prefix, distance=1) so typos still match.
fn build_hybrid_query(
    raw: &str,
    index: &Index,
    fields: &SchemaFields,
) -> Box<dyn tantivy::query::Query> {
    let mut parser = QueryParser::for_index(index, vec![fields.title, fields.content]);
    parser.set_conjunction_by_default();
    let exact: Box<dyn tantivy::query::Query> = parser
        .parse_query(raw)
        .unwrap_or_else(|_| Box::new(AllQuery));
    let boosted_exact: Box<dyn tantivy::query::Query> = Box::new(BoostQuery::new(exact, 5.0));

    const MAX_FUZZY: usize = 8;
    const MIN_LEN: usize = 2;

    let mut fuzzy_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
    for term_str in raw
        .split_whitespace()
        .filter(|t| t.len() >= MIN_LEN)
        .take(MAX_FUZZY)
    {
        let title_term = Term::from_field_text(fields.title, term_str);
        let content_term = Term::from_field_text(fields.content, term_str);
        let title_q: Box<dyn tantivy::query::Query> =
            Box::new(FuzzyTermQuery::new_prefix(title_term, 1, true));
        let content_q: Box<dyn tantivy::query::Query> =
            Box::new(FuzzyTermQuery::new_prefix(content_term, 1, true));
        let or_q: Box<dyn tantivy::query::Query> = Box::new(BooleanQuery::new(vec![
            (Occur::Should, title_q),
            (Occur::Should, content_q),
        ]));
        fuzzy_clauses.push((Occur::Should, or_q));
    }

    if fuzzy_clauses.is_empty() {
        return boosted_exact;
    }

    let fuzzy_q: Box<dyn tantivy::query::Query> = Box::new(BooleanQuery::new(fuzzy_clauses));
    Box::new(BooleanQuery::new(vec![
        (Occur::Should, boosted_exact),
        (Occur::Should, fuzzy_q),
    ]))
}

// ──────────────────────────────────────────────────────────────────────────────
// TantivyIndex public API
// ──────────────────────────────────────────────────────────────────────────────

impl TantivyIndex {
    /// Create a new `TantivyIndex` pointed at `index_path`.
    /// The index is not opened until `ensure_index` is called.
    pub fn new(index_path: PathBuf) -> Self {
        Self {
            index_path,
            inner: Mutex::new(None),
        }
    }

    /// Open (or create, or rebuild) the index.  Idempotent — safe to call
    /// multiple times; the second call is a no-op.
    pub fn ensure_index(&self) -> Result<()> {
        let mut guard = self.inner.lock();
        if guard.is_some() {
            return Ok(());
        }
        let (index, schema, fields) =
            open_or_create_index(&self.index_path).context("ensure_index")?;
        *guard = Some(TantivyState { index, schema, fields });
        Ok(())
    }

    /// Returns `{session_id: (mtime, agent)}` for all documents in the index.
    pub fn get_known_sessions(&self) -> Result<HashMap<String, (f64, String)>> {
        self.ensure_index()?;
        let guard = self.inner.lock();
        let state = guard.as_ref().expect("ensured above");

        let reader = state
            .index
            .reader()
            .context("opening index reader for get_known_sessions")?;
        let searcher = reader.searcher();
        let query = tantivy::query::AllQuery;

        let doc_addresses = searcher
            .search(&query, &DocSetCollector)
            .context("searching all docs for known sessions")?;

        let mut result = HashMap::new();
        for doc_addr in doc_addresses {
            let doc: TantivyDocument = searcher.doc(doc_addr).context("retrieving doc")?;
            if let (Some(id), Some(mtime), Some(agent)) = (
                doc.get_first(state.fields.id).and_then(|v| v.as_str()),
                doc.get_first(state.fields.mtime).and_then(|v| v.as_f64()),
                doc.get_first(state.fields.agent).and_then(|v| v.as_str()),
            ) {
                result.insert(id.to_string(), (mtime, agent.to_string()));
            }
        }
        Ok(result)
    }

    /// Returns all `Session` objects stored in the index.
    pub fn get_all_sessions(&self) -> Result<Vec<Session>> {
        self.ensure_index()?;
        let guard = self.inner.lock();
        let state = guard.as_ref().expect("ensured above");

        let reader = state
            .index
            .reader()
            .context("opening index reader for get_all_sessions")?;
        let searcher = reader.searcher();
        let query = tantivy::query::AllQuery;

        let doc_addresses = searcher
            .search(&query, &DocSetCollector)
            .context("searching all docs")?;

        let mut sessions = Vec::new();
        for doc_addr in doc_addresses {
            let doc: TantivyDocument = searcher.doc(doc_addr).context("retrieving doc")?;
            if let Some(session) = doc_to_session(&doc, &state.fields) {
                sessions.push(session);
            }
        }
        Ok(sessions)
    }

    /// Add or update sessions in the index (delete-by-id then re-add), then commit.
    pub fn update_sessions(&self, sessions: &[Session]) -> Result<()> {
        if sessions.is_empty() {
            return Ok(());
        }
        self.ensure_index()?;
        let guard = self.inner.lock();
        let state = guard.as_ref().expect("ensured above");

        let mut writer: IndexWriter = state
            .index
            .writer(WRITER_HEAP_BYTES)
            .context("creating index writer for update_sessions")?;

        for s in sessions {
            // Delete any existing document with the same id before re-adding.
            writer.delete_term(tantivy::Term::from_field_text(state.fields.id, &s.id));
            writer
                .add_document(session_to_doc(s, &state.fields))
                .context("adding document to index")?;
        }
        writer.commit().context("committing update_sessions")?;
        Ok(())
    }

    /// Delete sessions by id, then commit.
    pub fn delete_sessions(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.ensure_index()?;
        let guard = self.inner.lock();
        let state = guard.as_ref().expect("ensured above");

        let mut writer: IndexWriter = state
            .index
            .writer(WRITER_HEAP_BYTES)
            .context("creating index writer for delete_sessions")?;

        for id in ids {
            writer.delete_term(tantivy::Term::from_field_text(state.fields.id, id));
        }
        writer.commit().context("committing delete_sessions")?;
        Ok(())
    }

    /// Basic keyword search using BM25 via QueryParser.
    /// Returns `(session_id, score)` pairs sorted by descending score.
    pub fn search(&self, query_text: &str, limit: usize) -> Result<Vec<(String, f32)>> {
        if query_text.trim().is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_index()?;
        let guard = self.inner.lock();
        let state = guard.as_ref().expect("ensured above");

        let reader = state
            .index
            .reader()
            .context("opening index reader for search")?;
        let searcher = reader.searcher();

        let mut query_parser =
            QueryParser::for_index(&state.index, vec![state.fields.title, state.fields.content]);
        query_parser.set_conjunction_by_default();

        let query = query_parser
            .parse_query(query_text)
            .unwrap_or_else(|_| Box::new(tantivy::query::AllQuery));

        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .context("executing search query")?;

        let mut results = Vec::new();
        for (score, doc_addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_addr).context("retrieving search doc")?;
            if let Some(id) = doc.get_first(state.fields.id).and_then(|v| v.as_str()) {
                results.push((id.to_string(), score));
            }
        }
        Ok(results)
    }

    /// Search with optional agent/directory/date filters and hybrid BM25 + fuzzy ranking.
    ///
    /// Any `None` filter is ignored (all docs pass that filter axis).  When
    /// `query_text` is empty only the filters are applied.  Results are returned
    /// as `(session_id, score)` sorted by descending score.
    pub fn search_with_filters(
        &self,
        query_text: &str,
        agent_filter: Option<&Filter>,
        directory_filter: Option<&Filter>,
        date_filter: Option<&DateFilter>,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        self.ensure_index()?;
        let guard = self.inner.lock();
        let state = guard.as_ref().expect("ensured above");

        let reader = state
            .index
            .reader()
            .context("opening index reader for search_with_filters")?;
        let searcher = reader.searcher();

        // Build individual filter sub-queries.
        let mut must_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

        // ── Agent filter ────────────────────────────────────────────────────
        if let Some(af) = agent_filter {
            if !af.is_empty() {
                if !af.include.is_empty() {
                    let terms: Vec<Term> = af
                        .include
                        .iter()
                        .map(|v| Term::from_field_text(state.fields.agent, v))
                        .collect();
                    let q: Box<dyn tantivy::query::Query> = Box::new(TermSetQuery::new(terms));
                    must_clauses.push((Occur::Must, q));
                }
                for excl in &af.exclude {
                    let term = Term::from_field_text(state.fields.agent, excl);
                    let q: Box<dyn tantivy::query::Query> =
                        Box::new(TermSetQuery::new(vec![term]));
                    must_clauses.push((Occur::MustNot, q));
                }
            }
        }

        // ── Directory filter ────────────────────────────────────────────────
        if let Some(df) = directory_filter {
            if !df.is_empty() {
                for incl in &df.include {
                    let pattern = format!(
                        "(?i).*{}.*",
                        regex::escape(incl)
                    );
                    if let Ok(q) =
                        RegexQuery::from_pattern(&pattern, state.fields.directory)
                    {
                        must_clauses.push((Occur::Must, Box::new(q)));
                    }
                }
                for excl in &df.exclude {
                    let pattern = format!(
                        "(?i).*{}.*",
                        regex::escape(excl)
                    );
                    if let Ok(q) =
                        RegexQuery::from_pattern(&pattern, state.fields.directory)
                    {
                        must_clauses.push((Occur::MustNot, Box::new(q)));
                    }
                }
            }
        }

        // ── Date filter ─────────────────────────────────────────────────────
        if let Some(date_f) = date_filter {
            let cutoff_f64 = date_f.cutoff.as_second() as f64
                + (date_f.cutoff.subsec_nanosecond() as f64 / 1_000_000_000.0);

            let range_q: Box<dyn tantivy::query::Query> = match date_f.op {
                DateOp::Exact | DateOp::LessThan => {
                    // Sessions with timestamp >= cutoff (newer than cutoff).
                    Box::new(RangeQuery::new(
                        Bound::Included(Term::from_field_f64(
                            state.fields.timestamp,
                            cutoff_f64,
                        )),
                        Bound::Unbounded,
                    ))
                }
                DateOp::GreaterThan => {
                    // Sessions with timestamp <= cutoff (older than cutoff).
                    Box::new(RangeQuery::new(
                        Bound::Unbounded,
                        Bound::Included(Term::from_field_f64(
                            state.fields.timestamp,
                            cutoff_f64,
                        )),
                    ))
                }
            };

            if date_f.negated {
                must_clauses.push((Occur::MustNot, range_q));
            } else {
                must_clauses.push((Occur::Must, range_q));
            }
        }

        // ── Text search (hybrid BM25 + fuzzy) ───────────────────────────────
        let text_q: Box<dyn tantivy::query::Query> = if query_text.trim().is_empty() {
            Box::new(AllQuery)
        } else {
            build_hybrid_query(query_text, &state.index, &state.fields)
        };

        // ── Combine everything ───────────────────────────────────────────────
        let final_query: Box<dyn tantivy::query::Query> = if must_clauses.is_empty() {
            text_q
        } else {
            must_clauses.push((Occur::Must, text_q));
            Box::new(BooleanQuery::new(must_clauses))
        };

        let top_docs = searcher
            .search(
                final_query.as_ref(),
                &TopDocs::with_limit(limit).order_by_score(),
            )
            .context("executing search_with_filters query")?;

        let mut results = Vec::new();
        for (score, doc_addr) in top_docs {
            let doc: TantivyDocument =
                searcher.doc(doc_addr).context("retrieving search doc")?;
            if let Some(id) = doc.get_first(state.fields.id).and_then(|v| v.as_str()) {
                results.push((id.to_string(), score));
            }
        }
        Ok(results)
    }

    /// Wipe the index directory and force a full rebuild on the next access.
    pub fn clear(&self) -> Result<()> {
        let mut guard = self.inner.lock();
        // Drop the live state so the Mmap is released before we delete.
        *guard = None;
        if self.index_path.exists() {
            std::fs::remove_dir_all(&self.index_path)
                .context("clearing index directory")?;
        }
        Ok(())
    }

    /// Returns the total number of documents in the index.
    pub fn get_session_count(&self) -> Result<u64> {
        self.ensure_index()?;
        let guard = self.inner.lock();
        let state = guard.as_ref().expect("ensured above");

        let reader = state
            .index
            .reader()
            .context("opening index reader for get_session_count")?;
        Ok(reader.searcher().num_docs())
    }
}
