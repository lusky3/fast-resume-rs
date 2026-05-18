/// Tantivy full-text search index for fast-resume sessions.
///
/// Ported from python/fast_resume/index.py.
/// Schema version bumped to 22 for the Rust port.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tantivy::collector::{DocSetCollector, TopDocs};
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, STORED, TEXT,
};
use tantivy::schema::Value as TantivyValue;
use tantivy::{Index, IndexWriter, TantivyDocument};

use crate::config::SCHEMA_VERSION;
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
