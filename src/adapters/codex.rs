/// Codex CLI session adapter.
///
/// Faithfully ported from python/fast_resume/adapters/codex.py.
/// Sessions live under ~/.codex/sessions/ in YYYY/MM/DD/ subdirectories.

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
    format!("{}...", &text[..max_length])
}

pub struct CodexAdapter {
    sessions_dir: PathBuf,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            sessions_dir: config::codex_dir(),
        }
    }

    pub fn with_dir(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// Extract session ID from file content (session_meta event) or fall back
    /// to the filename stem.
    fn get_session_id_from_file(&self, path: &Path) -> String {
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        if let Ok(file) = std::fs::File::open(path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(data) = serde_json::from_str::<Value>(&trimmed) {
                    if data.get("type").and_then(|v| v.as_str()) == Some("session_meta") {
                        let id = data
                            .get("payload")
                            .and_then(|p| p.get("id"))
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
        // Fallback: filename stem, stripping leading "rollout-" prefix portion.
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if let Some(pos) = stem.find('-') {
            stem[pos + 1..].to_string()
        } else {
            stem
        }
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

        // path comes from adapter's own walkdir scan; not user-supplied.
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);

        let mut session_id = String::new();
        let mut directory = String::new();
        let mut messages: Vec<String> = Vec::new();
        let mut user_prompts: Vec<String> = Vec::new();
        let mut turn_count: u32 = 0;
        let mut yolo = false;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let data: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let payload = data.get("payload").cloned().unwrap_or(Value::Object(Default::default()));

            if msg_type == "session_meta" {
                if session_id.is_empty() {
                    session_id = payload.get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
                if directory.is_empty() {
                    directory = payload.get("cwd")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
            }

            if msg_type == "turn_context" {
                let approval_policy = payload.get("approval_policy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let sandbox_mode = payload.get("sandbox_policy")
                    .and_then(|v| v.as_object())
                    .and_then(|m| m.get("mode"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if approval_policy == "never" || sandbox_mode == "danger-full-access" {
                    yolo = true;
                }
            }

            if msg_type == "response_item" {
                let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if role == "user" || role == "assistant" {
                    let role_prefix = if role == "user" { "» " } else { "  " };
                    let content = payload.get("content").cloned().unwrap_or(Value::Array(vec![]));
                    let mut has_text = false;
                    if let Value::Array(parts) = content {
                        for part in parts {
                            if let Value::Object(obj) = &part {
                                let text = obj.get("text")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| obj.get("input_text").and_then(|v| v.as_str()))
                                    .unwrap_or("");
                                if !text.is_empty()
                                    && !text.trim().starts_with("<environment_context>")
                                {
                                    messages.push(format!("{}{}", role_prefix, text));
                                    has_text = true;
                                }
                            }
                        }
                    }
                    if has_text {
                        turn_count += 1;
                    }
                }
            }

            if msg_type == "event_msg" {
                let event_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if event_type == "user_message" {
                    let msg = payload.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    if !msg.is_empty() {
                        messages.push(format!("» {}", msg));
                        user_prompts.push(msg.to_string());
                    }
                } else if event_type == "agent_reasoning" {
                    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    if !text.is_empty() {
                        messages.push(format!("  {}", text));
                    }
                }
            }
        }

        // Must have at least one user prompt to index.
        if user_prompts.is_empty() {
            return Ok(None);
        }

        // Fall back to filename if no session_meta found.
        if session_id.is_empty() {
            session_id = self.get_session_id_from_file(path);
        }

        let title = truncate_title(&user_prompts[0], 80);
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
            yolo,
        }))
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn color(&self) -> &'static str {
        "#00A67E"
    }

    fn badge(&self) -> &'static str {
        "codex"
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
        let mut cmd = vec!["codex".to_string()];
        if yolo {
            cmd.push("--dangerously-bypass-approvals-and-sandbox".to_string());
        }
        cmd.push("resume".to_string());
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
