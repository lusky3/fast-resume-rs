/// Crush (charmbracelet) session adapter.
///
/// Ported from python/fast_resume/adapters/crush.py.
/// Sessions live in SQLite databases (`crush.db`) per project.
/// Projects are enumerated from `~/.local/share/crush/projects.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::adapters::{AgentAdapter, ErrorCb, IncrementalResult, SessionCb};
use crate::config;
use crate::session::{ParseError, RawAdapterStats, Session};

pub struct CrushAdapter {
    projects_file: PathBuf,
}

impl CrushAdapter {
    pub fn new() -> Self {
        Self {
            projects_file: config::crush_projects_file(),
        }
    }

    /// Construct with a custom projects.json path (for tests).
    pub fn with_projects_file(projects_file: PathBuf) -> Self {
        Self { projects_file }
    }

    /// Parse the projects.json and return a list of `(project_path, db_path)` tuples.
    fn load_projects(&self) -> Vec<(String, PathBuf)> {
        let content = match std::fs::read(&self.projects_file) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let data: Value = match serde_json::from_slice(&content) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };

        let projects = match data.get("projects").and_then(|v| v.as_array()) {
            Some(arr) => arr.clone(),
            None => return Vec::new(),
        };

        let mut result = Vec::new();
        for project in &projects {
            let project_path = project
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let data_dir = project
                .get("data_dir")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if data_dir.is_empty() {
                continue;
            }

            let db_path = Path::new(data_dir).join("crush.db");
            if db_path.exists() {
                result.push((project_path, db_path));
            }
        }
        result
    }

    /// Load sessions from a single Crush SQLite database.
    fn load_sessions_from_db(
        &self,
        db_path: &Path,
        project_path: &str,
        on_error: Option<ErrorCb<'_>>,
    ) -> Vec<Session> {
        match self.try_load_sessions_from_db(db_path, project_path) {
            Ok(sessions) => sessions,
            Err(e) => {
                let err = ParseError {
                    agent: self.name().to_string(),
                    file_path: db_path.display().to_string(),
                    error_type: "rusqlite::Error".to_string(),
                    message: e.to_string(),
                };
                if let Some(cb) = on_error {
                    cb(err);
                }
                Vec::new()
            }
        }
    }

    fn try_load_sessions_from_db(
        &self,
        db_path: &Path,
        project_path: &str,
    ) -> Result<Vec<Session>> {
        let conn = rusqlite::Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;

        // Fetch session metadata and messages in one JOIN query, ordered so we
        // can group by session_id as we iterate.
        let mut stmt = conn.prepare(
            "SELECT
                 s.id        AS session_id,
                 s.title     AS session_title,
                 s.updated_at,
                 s.created_at,
                 s.message_count AS msg_count,
                 m.role,
                 m.parts,
                 m.created_at AS msg_created_at
             FROM sessions s
             LEFT JOIN messages m ON m.session_id = s.id
             WHERE s.message_count > 0
             ORDER BY s.updated_at DESC, m.created_at ASC",
        )?;

        // Group data by session_id.
        // Use a Vec to preserve ORDER BY updated_at DESC; a HashMap alone would lose order.
        let mut session_order: Vec<String> = Vec::new();
        let mut session_meta: HashMap<String, (String, i64, i64)> = HashMap::new();
        let mut session_messages: HashMap<String, Vec<(String, String)>> = HashMap::new();

        let rows = stmt.query_map([], |row| {
            let session_id: String = row.get(0)?;
            let title: Option<String> = row.get(1)?;
            let updated_at: i64 = row.get(2).unwrap_or(0);
            let created_at: i64 = row.get(3).unwrap_or(0);
            let role: Option<String> = row.get(5)?;
            let parts: Option<String> = row.get(6)?;
            Ok((
                session_id,
                title.unwrap_or_default(),
                updated_at,
                created_at,
                role,
                parts.unwrap_or_default(),
            ))
        })?;

        for row_result in rows {
            let (session_id, title, updated_at, created_at, role, parts) =
                match row_result {
                    Ok(r) => r,
                    Err(_) => continue,
                };

            if !session_meta.contains_key(&session_id) {
                session_order.push(session_id.clone());
                session_meta.insert(session_id.clone(), (title, updated_at, created_at));
            }

            if let Some(role) = role {
                session_messages
                    .entry(session_id)
                    .or_default()
                    .push((role, parts));
            }
        }

        let mut sessions = Vec::new();
        for session_id in &session_order {
            let (title, updated_at, created_at) = match session_meta.get(session_id) {
                Some(v) => v,
                None => continue,
            };
            if let Some(session) = self.build_session(
                session_id,
                title,
                *updated_at,
                *created_at,
                session_messages
                    .get(session_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
                project_path,
            ) {
                sessions.push(session);
            }
        }

        Ok(sessions)
    }

    fn build_session(
        &self,
        session_id: &str,
        title: &str,
        updated_at: i64,
        created_at: i64,
        messages_raw: &[(String, String)],
        project_path: &str,
    ) -> Option<Session> {
        // Timestamps: Crush stores either seconds or milliseconds.
        // Values > 1e11 are assumed to be milliseconds.
        let normalize_ts = |ts: i64| -> f64 {
            if ts > 100_000_000_000 {
                ts as f64 / 1000.0
            } else {
                ts as f64
            }
        };

        let ts_secs = normalize_ts(if updated_at != 0 { updated_at } else { created_at });
        let timestamp = f64_to_timestamp(ts_secs);

        let mut messages: Vec<String> = Vec::new();
        let mut first_user_message = String::new();

        for (role, parts_json) in messages_raw {
            let text = extract_text_from_parts(parts_json);
            if text.is_empty() {
                continue;
            }
            let role_prefix = if role == "user" { "» " } else { "  " };
            messages.push(format!("{}{}", role_prefix, text));

            if role == "user" && first_user_message.is_empty() && text.len() > 5 {
                first_user_message = text;
            }
        }

        if messages.is_empty() || first_user_message.is_empty() {
            return None;
        }

        let final_title = if title.trim().is_empty() {
            truncate_title(&first_user_message, 100)
        } else {
            title.to_string()
        };

        Some(Session {
            id: session_id.to_string(),
            agent: self.name().to_string(),
            title: final_title,
            directory: project_path.to_string(),
            timestamp,
            content: messages.join("\n\n"),
            message_count: messages.len() as u32,
            mtime: ts_secs,
            yolo: false,
        })
    }
}

impl Default for CrushAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Shared helpers ─────────────────────────────────────────────────────────────

fn f64_to_timestamp(secs: f64) -> jiff::Timestamp {
    let whole = secs as i64;
    let nanos = ((secs - whole as f64) * 1_000_000_000.0) as i32;
    jiff::Timestamp::new(whole, nanos).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

fn truncate_title(text: &str, max_length: usize) -> String {
    let text = text.trim();
    if text.len() <= max_length {
        return text.to_string();
    }
    format!("{}...", &text[..max_length])
}

/// Extract plain text from Crush's parts JSON string.
///
/// The parts column is a JSON array of objects with a `type` and `data` field:
/// `[{"type": "text", "data": {"text": "..."}}, ...]`
fn extract_text_from_parts(parts_json: &str) -> String {
    if parts_json.is_empty() {
        return String::new();
    }
    let parts: Value = match serde_json::from_str(parts_json) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let arr = match parts.as_array() {
        Some(a) => a,
        None => return String::new(),
    };

    let mut text_parts: Vec<String> = Vec::new();
    for part in arr {
        let obj = match part.as_object() {
            Some(o) => o,
            None => continue,
        };
        let part_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let data = obj.get("data");

        match part_type {
            "text" => {
                if let Some(text) = data
                    .and_then(|d| d.get("text"))
                    .and_then(|v| v.as_str())
                {
                    if !text.is_empty() {
                        text_parts.push(text.to_string());
                    }
                }
            }
            "tool_result" => {
                if let Some(content) = data.and_then(|d| d.get("content")).and_then(|v| v.as_str())
                {
                    if !content.is_empty() && content.len() < 500 {
                        let name = data
                            .and_then(|d| d.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool");
                        text_parts.push(format!("[{}]: {}", name, &content[..content.len().min(200)]));
                    }
                }
            }
            "tool_call" => {
                if let Some(name) = data.and_then(|d| d.get("name")).and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        text_parts.push(format!("[calling {}]", name));
                    }
                }
            }
            _ => {}
        }
    }
    text_parts.join(" ")
}

// ── AgentAdapter implementation ───────────────────────────────────────────────

impl AgentAdapter for CrushAdapter {
    fn name(&self) -> &'static str {
        "crush"
    }

    fn color(&self) -> &'static str {
        "#6B51FF"
    }

    fn badge(&self) -> &'static str {
        "crush"
    }

    fn supports_yolo(&self) -> bool {
        false
    }

    fn is_available(&self) -> bool {
        self.projects_file.exists()
    }

    fn find_sessions(&self) -> Vec<Session> {
        if !self.is_available() {
            return Vec::new();
        }
        let mut sessions = Vec::new();
        for (project_path, db_path) in self.load_projects() {
            let project_sessions = self.load_sessions_from_db(&db_path, &project_path, None);
            sessions.extend(project_sessions);
        }
        sessions
    }

    fn find_sessions_incremental(
        &self,
        known: &HashMap<String, (f64, String)>,
        on_error: Option<ErrorCb<'_>>,
        on_session: Option<SessionCb<'_>>,
    ) -> IncrementalResult {
        if !self.is_available() {
            let deleted_ids = known
                .iter()
                .filter(|(_, (_, agent))| agent == self.name())
                .map(|(id, _)| id.clone())
                .collect();
            return IncrementalResult {
                new_or_modified: vec![],
                deleted_ids,
            };
        }

        let mut new_or_modified = Vec::new();
        let mut all_current_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for (project_path, db_path) in self.load_projects() {
            let project_sessions =
                self.load_sessions_from_db(&db_path, &project_path, on_error);
            for session in project_sessions {
                all_current_ids.insert(session.id.clone());
                let session_mtime = session.mtime;
                let known_entry = known.get(&session.id);
                if known_entry.is_none()
                    || session_mtime > known_entry.unwrap().0 + crate::adapters::MTIME_TOLERANCE
                {
                    if let Some(cb) = on_session {
                        cb(session.clone());
                    }
                    new_or_modified.push(session);
                }
            }
        }

        let deleted_ids = known
            .iter()
            .filter(|(id, (_, agent))| {
                agent == self.name() && !all_current_ids.contains(*id)
            })
            .map(|(id, _)| id.clone())
            .collect();

        IncrementalResult {
            new_or_modified,
            deleted_ids,
        }
    }

    fn get_resume_command(&self, _session: &Session, _yolo: bool) -> Vec<String> {
        // Crush has no CLI resume command. The TUI will show a toast (Phase 6).
        // Returning an empty vec signals to the caller that resuming is unsupported.
        vec!["crush".to_string()]
    }

    fn get_raw_stats(&self) -> RawAdapterStats {
        let data_dir = self
            .projects_file
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        if !self.is_available() {
            return RawAdapterStats {
                agent: self.name().to_string(),
                data_dir,
                available: false,
                file_count: 0,
                total_bytes: 0,
            };
        }

        let projects = self.load_projects();
        let file_count = projects.len() as u64;
        let total_bytes: u64 = projects
            .iter()
            .map(|(_, db_path)| db_path.metadata().map(|m| m.len()).unwrap_or(0))
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
