/// Gemini CLI session adapter.
///
/// Ported from python/fast_resume/adapters/gemini.py.
///
/// Gemini stores chats per project under `~/.gemini/tmp/<project-slug>/chats/`.
/// Two formats coexist:
///   session-<ts>-<short>.json   Single JSON object with `sessionId` + `messages[]`.
///   session-<ts>-<short>.jsonl  Streaming JSONL: first line is the session metadata,
///                                subsequent lines are message objects interleaved with
///                                `{"$set": {...}}` update patches.
///
/// Working directories are recovered from `~/.gemini/projects.json`, which maps
/// directory paths to project slugs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::adapters::{file_based_incremental, AgentAdapter, ErrorCb, IncrementalResult, SessionCb};
use crate::config;
use crate::session::{ParseError, RawAdapterStats, Session};

pub struct GeminiAdapter {
    /// The root `~/.gemini` directory. Chats live under `tmp/<slug>/chats/`.
    sessions_dir: PathBuf,
}

impl GeminiAdapter {
    pub fn new() -> Self {
        Self {
            sessions_dir: config::gemini_dir(),
        }
    }

    /// Construct with a custom sessions directory (for tests).
    pub fn with_dir(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    fn chats_root(&self) -> PathBuf {
        self.sessions_dir.join("tmp")
    }

    fn projects_file(&self) -> PathBuf {
        self.sessions_dir.join("projects.json")
    }

    /// Load `{slug: directory}` from `projects.json`.
    ///
    /// The file stores `{directory: slug}` pairs — we invert for efficient lookup.
    fn load_project_dirs(&self) -> HashMap<String, String> {
        let pf = self.projects_file();
        if !pf.exists() {
            return HashMap::new();
        }
        let content = match std::fs::read(&pf) {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };
        let data: Value = match serde_json::from_slice(&content) {
            Ok(v) => v,
            Err(_) => return HashMap::new(),
        };

        let mut result = HashMap::new();
        if let Some(projects) = data.get("projects").and_then(|v| v.as_object()) {
            for (directory, slug) in projects {
                if let Some(slug_str) = slug.as_str() {
                    result.insert(slug_str.to_string(), directory.clone());
                }
            }
        }
        result
    }

    /// Enumerate all session files under `~/.gemini/tmp/*/chats/session-*.{json,jsonl}`.
    /// Returns `(session_id, path, mtime)` tuples, deduplicated by session_id
    /// (keeping whichever file has the later mtime).
    fn scan(&self) -> Vec<(String, PathBuf, f64)> {
        let mut by_id: HashMap<String, (PathBuf, f64)> = HashMap::new();

        let chats_root = self.chats_root();
        let slug_dirs = match std::fs::read_dir(&chats_root) {
            Ok(rd) => rd,
            Err(_) => return Vec::new(),
        };

        for slug_entry in slug_dirs.flatten() {
            let chats_dir = slug_entry.path().join("chats");
            if !chats_dir.is_dir() {
                continue;
            }
            let chat_entries = match std::fs::read_dir(&chats_dir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            for chat_entry in chat_entries.flatten() {
                let path = chat_entry.path();
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !matches!(ext, "json" | "jsonl") {
                    continue;
                }
                let stem = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s,
                    None => continue,
                };
                if !stem.starts_with("session-") {
                    continue;
                }

                let mtime = match path.metadata().and_then(|m| m.modified()) {
                    Ok(t) => t.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
                    Err(_) => continue,
                };

                let session_id = session_id_quick(&path);

                let entry = by_id.entry(session_id).or_insert((path.clone(), 0.0));
                if mtime > entry.1 {
                    *entry = (path, mtime);
                }
            }
        }

        by_id
            .into_iter()
            .map(|(id, (path, mtime))| (id, path, mtime))
            .collect()
    }

    pub fn parse_session_file(
        &self,
        path: &Path,
        on_error: Option<ErrorCb<'_>>,
    ) -> Option<Session> {
        let slug_to_dir = self.load_project_dirs();
        self.parse_file(path, &slug_to_dir, on_error)
    }

    fn parse_file(
        &self,
        session_file: &Path,
        slug_to_dir: &HashMap<String, String>,
        on_error: Option<ErrorCb<'_>>,
    ) -> Option<Session> {
        let ext = session_file.extension().and_then(|e| e.to_str()).unwrap_or("");
        let result = if ext == "jsonl" {
            parse_jsonl(session_file)
        } else {
            parse_json(session_file)
        };

        let (meta, messages) = match result {
            Ok(pair) => pair,
            Err(e) => {
                if let Some(cb) = on_error {
                    cb(ParseError {
                        agent: self.name().to_string(),
                        file_path: session_file.display().to_string(),
                        error_type: "ParseError".to_string(),
                        message: e.to_string(),
                    });
                }
                return None;
            }
        };

        let session_id = meta
            .get("sessionId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| id_from_filename(session_file));

        if session_id.is_empty() {
            return None;
        }

        // Derive the working directory from the project slug.
        let slug = slug_for(session_file);
        let directory = slug_to_dir.get(&slug).cloned().unwrap_or_default();

        // Parse timestamp from metadata.
        let fallback_mtime = session_file
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let ts_str = meta
            .get("lastUpdated")
            .or_else(|| meta.get("startTime"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let timestamp = if !ts_str.is_empty() {
            parse_iso_timestamp(ts_str, fallback_mtime)
        } else {
            f64_to_timestamp(fallback_mtime)
        };

        let mut display_messages: Vec<String> = Vec::new();
        let mut first_user_prompt = String::new();
        let mut turn_count: u32 = 0;

        for (role, text) in &messages {
            if text.is_empty() {
                continue;
            }
            if role == "user" {
                display_messages.push(format!("» {}", text));
                turn_count += 1;
                if first_user_prompt.is_empty() {
                    first_user_prompt = text.clone();
                }
            } else {
                // assistant ("gemini")
                display_messages.push(format!("  {}", text));
                turn_count += 1;
            }
        }

        if first_user_prompt.is_empty() {
            // Sessions with no real user message are not useful to resume.
            return None;
        }

        let title = truncate_title(&first_user_prompt, 80);

        Some(Session {
            id: session_id,
            agent: self.name().to_string(),
            title,
            directory,
            timestamp,
            content: display_messages.join("\n\n"),
            message_count: turn_count,
            mtime: fallback_mtime,
            yolo: false,
        })
    }
}

impl Default for GeminiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// ── File parsers ───────────────────────────────────────────────────────────────

/// Parse the legacy single-JSON session format.
fn parse_json(session_file: &Path) -> anyhow::Result<(Value, Vec<(String, String)>)> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let content = std::fs::read(session_file)?;
    let payload: Value = serde_json::from_slice(&content)?;
    let meta = serde_json::json!({
        "sessionId": payload.get("sessionId"),
        "startTime": payload.get("startTime"),
        "lastUpdated": payload.get("lastUpdated"),
    });
    let messages_arr = payload
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let messages: Vec<(String, String)> = messages_arr
        .iter()
        .filter_map(classify)
        .collect();
    Ok((meta, messages))
}

/// Parse the streaming JSONL session format.
fn parse_jsonl(session_file: &Path) -> anyhow::Result<(Value, Vec<(String, String)>)> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let file = std::fs::File::open(session_file)?;
    let reader = BufReader::new(file);

    let mut meta_map: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut messages: Vec<(String, String)> = Vec::new();
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let obj = match entry.as_object() {
            Some(o) => o,
            None => continue,
        };

        if obj.contains_key("$set") {
            // Merge only known timestamp fields to prevent session-id swapping.
            if let Some(patch) = obj.get("$set").and_then(|v| v.as_object()) {
                for key in &["startTime", "lastUpdated"] {
                    if let Some(val) = patch.get(*key) {
                        meta_map.insert(key.to_string(), val.clone());
                    }
                }
            }
            continue;
        }

        if obj.contains_key("sessionId") && !obj.contains_key("messages") {
            // First-line session header.
            for key in &["sessionId", "startTime", "lastUpdated"] {
                if let Some(val) = obj.get(*key) {
                    meta_map.insert(key.to_string(), val.clone());
                }
            }
            continue;
        }

        // Deduplicate repeated message rows.
        if let Some(msg_id) = obj.get("id").and_then(|v| v.as_str()) {
            if !msg_id.is_empty() {
                if seen_ids.contains(msg_id) {
                    continue;
                }
                seen_ids.insert(msg_id.to_string());
            }
        }

        if let Some(pair) = classify(&entry) {
            messages.push(pair);
        }
    }

    Ok((Value::Object(meta_map), messages))
}

/// Map a Gemini message object to `(role, text)` or `None` if not indexable.
fn classify(msg: &Value) -> Option<(String, String)> {
    let obj = msg.as_object()?;
    let msg_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(msg_type, "user" | "gemini") {
        return None; // Skip info/error/system entries.
    }

    let text = match obj.get("content") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(parts)) => {
            let mut texts = Vec::new();
            for part in parts {
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    let t = t.trim();
                    if !t.is_empty() {
                        texts.push(t.to_string());
                    }
                }
            }
            texts.join("\n")
        }
        _ => String::new(),
    };

    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }

    let role = if msg_type == "user" {
        "user".to_string()
    } else {
        "assistant".to_string()
    };
    Some((role, text))
}

// ── Session-ID extraction without a full parse ────────────────────────────────

/// Maximum number of lines scanned in a JSONL looking for the session header.
const MAX_HEADER_SCAN_LINES: usize = 32;

fn session_id_quick(session_file: &Path) -> String {
    let fallback = id_from_filename(session_file);
    let ext = session_file.extension().and_then(|e| e.to_str()).unwrap_or("");
    match std::fs::File::open(session_file) {
        Err(_) => fallback,
        Ok(file) => {
            if ext == "jsonl" {
                let reader = BufReader::new(file);
                let mut count = 0;
                for line in reader.lines() {
                    if count >= MAX_HEADER_SCAN_LINES {
                        break;
                    }
                    count += 1;
                    let line = match line {
                        Ok(l) => l,
                        Err(_) => continue,
                    };
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let entry: Value = match serde_json::from_str(trimmed) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let obj = match entry.as_object() {
                        Some(o) => o,
                        None => continue,
                    };
                    if obj.contains_key("$set") {
                        continue;
                    }
                    if let Some(sid) = obj.get("sessionId").and_then(|v| v.as_str()) {
                        if !sid.is_empty() {
                            return sid.to_string();
                        }
                    }
                }
                fallback
            } else {
                // Single-JSON file — read whole file.
                use std::io::Read;
                let mut buf = Vec::new();
                let mut f = file;
                if f.read_to_end(&mut buf).is_err() {
                    return fallback;
                }
                let payload: Value = match serde_json::from_slice(&buf) {
                    Ok(v) => v,
                    Err(_) => return fallback,
                };
                payload
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or(fallback)
            }
        }
    }
}

fn id_from_filename(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Extract the project slug from the session file path.
/// Path pattern: `…/tmp/<slug>/chats/<file>`.
fn slug_for(session_file: &Path) -> String {
    session_file
        .parent()         // chats/
        .and_then(|p| p.parent()) // <slug>/
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

// ── Small utilities ───────────────────────────────────────────────────────────

fn f64_to_timestamp(secs: f64) -> jiff::Timestamp {
    let whole = secs as i64;
    let nanos = ((secs - whole as f64) * 1_000_000_000.0) as i32;
    jiff::Timestamp::new(whole, nanos).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

fn parse_iso_timestamp(s: &str, fallback_mtime: f64) -> jiff::Timestamp {
    let normalized = if s.ends_with('Z') {
        format!("{}{}", &s[..s.len() - 1], "+00:00")
    } else {
        s.to_string()
    };
    if let Ok(ts) = normalized.parse::<jiff::Timestamp>() {
        return ts;
    }
    f64_to_timestamp(fallback_mtime)
}

fn truncate_title(text: &str, max_length: usize) -> String {
    let text = text.trim();
    if text.len() <= max_length {
        return text.to_string();
    }
    format!("{}...", &text[..max_length])
}

// ── AgentAdapter implementation ───────────────────────────────────────────────

impl AgentAdapter for GeminiAdapter {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn color(&self) -> &'static str {
        "#4285F4"
    }

    fn badge(&self) -> &'static str {
        "gemini"
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.chats_root().exists()
    }

    fn find_sessions(&self) -> Vec<Session> {
        if !self.is_available() {
            return Vec::new();
        }
        let slug_to_dir = self.load_project_dirs();
        self.scan()
            .into_iter()
            .filter_map(|(_, path, _)| self.parse_file(&path, &slug_to_dir, None))
            .collect()
    }

    fn find_sessions_incremental(
        &self,
        known: &HashMap<String, (f64, String)>,
        on_error: Option<ErrorCb<'_>>,
        on_session: Option<SessionCb<'_>>,
    ) -> IncrementalResult {
        // Pre-load project dirs once instead of per file.
        let slug_to_dir = self.load_project_dirs();
        file_based_incremental(
            self.name(),
            self.is_available(),
            known,
            || self.scan(),
            |path, err_cb| self.parse_file(path, &slug_to_dir, err_cb),
            on_error,
            on_session,
        )
    }

    fn get_resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["gemini".to_string()];
        if yolo {
            cmd.push("--yolo".to_string());
        }
        cmd.push("--resume".to_string());
        cmd.push(session.id.clone());
        cmd
    }

    fn get_raw_stats(&self) -> RawAdapterStats {
        let data_dir = self.chats_root().display().to_string();
        if !self.is_available() {
            return RawAdapterStats {
                agent: self.name().to_string(),
                data_dir,
                available: false,
                file_count: 0,
                total_bytes: 0,
            };
        }

        let files = self.scan();
        let file_count = files.len() as u64;
        let total_bytes: u64 = files
            .iter()
            .map(|(_, path, _)| path.metadata().map(|m| m.len()).unwrap_or(0))
            .sum();

        RawAdapterStats {
            agent: self.name().to_string(),
            data_dir,
            available: true,
            file_count,
            total_bytes,
        }
    }
}
