/// GitHub Copilot CLI session adapter.
///
/// Faithfully ported from python/fast_resume/adapters/copilot.py.
/// Sessions live under ~/.copilot/session-state/ as **/*.jsonl.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;
use walkdir::WalkDir;

use crate::adapters::{file_based_incremental, AgentAdapter, ErrorCb, IncrementalResult, SessionCb};
use crate::config;
use crate::session::{ParseError, RawAdapterStats, Session};

fn system_time_to_f64(t: std::time::SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

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
    let truncated = &text[..max_length];
    match truncated.rfind(' ') {
        Some(pos) => format!("{}...", &truncated[..pos]),
        None => format!("{}...", truncated),
    }
}

pub struct CopilotAdapter {
    sessions_dir: PathBuf,
}

impl CopilotAdapter {
    pub fn new() -> Self {
        Self {
            sessions_dir: config::copilot_dir(),
        }
    }

    pub fn with_dir(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// Derive a fallback session id from the file path.
    ///
    /// For files in UUID subdirectories (e.g. `<UUID>/events.jsonl`), use the
    /// parent directory name.  For files directly in the session-state dir, use
    /// the file stem.
    fn fallback_session_id(&self, path: &Path) -> String {
        if let Some(parent) = path.parent() {
            if parent != self.sessions_dir {
                return parent
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
            }
        }
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    }

    /// Extract session ID from file content (session.start event) or fall back
    /// to filename/parent directory.
    fn get_session_id_from_file(&self, path: &Path) -> String {
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        if let Ok(file) = std::fs::File::open(path) {
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<Value>(&trimmed) {
                    if entry.get("type").and_then(|v| v.as_str()) == Some("session.start") {
                        let id = entry
                            .get("data")
                            .and_then(|d| d.get("sessionId"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !id.is_empty() {
                            return id;
                        }
                        break;
                    }
                }
            }
        }
        self.fallback_session_id(path)
    }

    fn scan(&self) -> Vec<(String, PathBuf, f64)> {
        let mut results = Vec::new();
        for entry in WalkDir::new(&self.sessions_dir)
            .into_iter()
            .flatten()
        {
            let path = entry.path().to_path_buf();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = match path.metadata().and_then(|m| m.modified()) {
                Ok(t) => system_time_to_f64(t),
                Err(_) => continue,
            };
            let session_id = self.get_session_id_from_file(&path);
            results.push((session_id, path, mtime));
        }
        results
    }

    pub fn parse_session_file(
        &self,
        path: &Path,
        on_error: Option<ErrorCb<'_>>,
    ) -> Option<Session> {
        match self.parse_session_file_inner(path) {
            Ok(session) => session,
            Err(e) => {
                if let Some(cb) = on_error {
                    cb(ParseError {
                        agent: self.name().to_string(),
                        file_path: path.display().to_string(),
                        error_type: "IOError".to_string(),
                        message: e.to_string(),
                    });
                }
                None
            }
        }
    }

    fn parse_session_file_inner(&self, path: &Path) -> anyhow::Result<Option<Session>> {
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .map(system_time_to_f64)
            .unwrap_or(0.0);

        let mut session_id = self.fallback_session_id(path);
        let mut first_user_message = String::new();
        let mut directory = String::new();
        let mut messages: Vec<String> = Vec::new();
        let mut turn_count: u32 = 0;

        // path comes from adapter's own walkdir scan; not user-supplied.
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);

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

            let msg_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let data = entry.get("data").cloned().unwrap_or(Value::Object(Default::default()));

            if msg_type == "session.start" {
                if let Some(id) = data.get("sessionId").and_then(|v| v.as_str()) {
                    if !id.is_empty() {
                        session_id = id.to_string();
                    }
                }
                if directory.is_empty() {
                    directory = data
                        .get("context")
                        .and_then(|c| c.get("cwd"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
            }

            // Older Copilot CLI: extract directory from folder_trust info.
            if msg_type == "session.info"
                && directory.is_empty()
                && data.get("infoType").and_then(|v| v.as_str()) == Some("folder_trust")
            {
                let message = data.get("message").and_then(|v| v.as_str()).unwrap_or("");
                // Extract path from "Folder /path/to/dir has been added..."
                if let Some(pos) = message.find("Folder /") {
                    let after = &message[pos + 7..];
                    let end = after.find(' ').unwrap_or(after.len());
                    directory = after[..end].to_string();
                }
            }

            if msg_type == "user.message" {
                let content = data.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if !content.is_empty() {
                    messages.push(format!("» {}", content));
                    turn_count += 1;
                    if first_user_message.is_empty() && content.len() > 10 {
                        first_user_message = content.to_string();
                    }
                }
            }

            if msg_type == "assistant.message" {
                let content = data.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if !content.is_empty() {
                    messages.push(format!("  {}", content));
                    turn_count += 1;
                }
            }
        }

        if first_user_message.is_empty() || messages.is_empty() {
            return Ok(None);
        }

        let title = truncate_title(&first_user_message, 100);
        let content = messages.join("\n\n");

        Ok(Some(Session {
            id: session_id,
            agent: self.name().to_string(),
            title,
            directory,
            timestamp: f64_to_timestamp(mtime),
            content,
            message_count: turn_count,
            mtime,
            yolo: false,
        }))
    }
}

impl Default for CopilotAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for CopilotAdapter {
    fn name(&self) -> &'static str {
        "copilot-cli"
    }

    fn color(&self) -> &'static str {
        "#9CA3AF"
    }

    fn badge(&self) -> &'static str {
        "copilot"
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.sessions_dir.exists()
    }

    fn find_sessions(&self) -> Vec<Session> {
        if !self.is_available() {
            return Vec::new();
        }
        self.scan()
            .into_iter()
            .filter_map(|(_, path, _)| self.parse_session_file(&path, None))
            .collect()
    }

    fn find_sessions_incremental(
        &self,
        known: &HashMap<String, (f64, String)>,
        on_error: Option<ErrorCb<'_>>,
        on_session: Option<SessionCb<'_>>,
    ) -> IncrementalResult {
        file_based_incremental(
            self.name(),
            self.is_available(),
            known,
            || self.scan(),
            |path, err_cb| self.parse_session_file(path, err_cb),
            on_error,
            on_session,
        )
    }

    fn get_resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["copilot".to_string()];
        if yolo {
            cmd.push("--allow-all-tools".to_string());
            cmd.push("--allow-all-paths".to_string());
        }
        cmd.push("--resume".to_string());
        cmd.push(session.id.clone());
        cmd
    }

    fn get_raw_stats(&self) -> RawAdapterStats {
        if !self.is_available() {
            return RawAdapterStats {
                agent: self.name().to_string(),
                data_dir: self.sessions_dir.display().to_string(),
                available: false,
                file_count: 0,
                total_bytes: 0,
            };
        }
        let files = self.scan();
        let file_count = files.len() as u64;
        let total_bytes: u64 = files
            .iter()
            .filter_map(|(_, path, _)| path.metadata().ok())
            .map(|m| m.len())
            .sum();
        RawAdapterStats {
            agent: self.name().to_string(),
            data_dir: self.sessions_dir.display().to_string(),
            available: true,
            file_count,
            total_bytes,
        }
    }
}
