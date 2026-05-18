/// Claude Code session adapter.
///
/// Faithfully ported from python/fast_resume/adapters/claude.py.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Context;
use serde_json::Value;

use crate::adapters::{file_based_incremental, AgentAdapter, ErrorCb, IncrementalResult, SessionCb};
use crate::config;
use crate::session::{ParseError, RawAdapterStats, Session};

/// Truncate title text with word-break, mirroring `truncate_title` in base.py.
fn truncate_title(text: &str, max_length: usize) -> String {
    let text = text.trim();
    if text.len() <= max_length {
        return text.to_string();
    }
    let truncated = &text[..max_length];
    // Break at last word boundary.
    let truncated = match truncated.rfind(' ') {
        Some(pos) => &truncated[..pos],
        None => truncated,
    };
    format!("{}...", truncated)
}

/// Convert `std::time::SystemTime` → seconds since UNIX epoch as `f64`.
fn system_time_to_f64(t: std::time::SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Convert `f64` seconds-since-epoch → `jiff::Timestamp`.
fn f64_to_timestamp(secs: f64) -> jiff::Timestamp {
    let whole = secs as i64;
    let nanos = ((secs - whole as f64) * 1_000_000_000.0) as i32;
    jiff::Timestamp::new(whole, nanos).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

pub struct ClaudeAdapter {
    sessions_dir: PathBuf,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self {
            sessions_dir: config::claude_dir(),
        }
    }

    /// Create an adapter pointing at a custom sessions directory (for tests).
    pub fn with_dir(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// Scan all Claude project directories for `*.jsonl` files, skipping `agent-*.jsonl`.
    ///
    /// Returns `Vec<(session_id, path, mtime_f64)>`.
    fn scan(&self) -> Vec<(String, PathBuf, f64)> {
        let mut results = Vec::new();

        let read_dir = match std::fs::read_dir(&self.sessions_dir) {
            Ok(rd) => rd,
            Err(_) => return results,
        };

        for entry in read_dir.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }

            let glob = match std::fs::read_dir(&project_dir) {
                Ok(g) => g,
                Err(_) => continue,
            };

            for file_entry in glob.flatten() {
                let path = file_entry.path();
                let file_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };

                // Only process .jsonl files, skipping agent subprocesses.
                if !file_name.ends_with(".jsonl") {
                    continue;
                }
                if file_name.starts_with("agent-") {
                    continue;
                }

                let mtime = match path.metadata().and_then(|m| m.modified()) {
                    Ok(t) => system_time_to_f64(t),
                    Err(_) => continue,
                };

                let session_id = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };

                results.push((session_id, path, mtime));
            }
        }

        results
    }

    /// Parse a single Claude JSONL session file.
    ///
    /// Returns `None` if the file has no usable content.
    pub fn parse_session_file(
        &self,
        path: &Path,
        on_error: Option<ErrorCb<'_>>,
    ) -> Option<Session> {
        match self.parse_session_file_inner(path) {
            Ok(session) => session,
            Err(e) => {
                let err = ParseError {
                    agent: self.name().to_string(),
                    file_path: path.display().to_string(),
                    error_type: "IOError".to_string(),
                    message: e.to_string(),
                };
                if let Some(cb) = on_error {
                    cb(err);
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

        // path comes from adapter's own directory scan of self.sessions_dir
        // (~/.claude/projects/), never from user-supplied input.
        let file = std::fs::File::open(path) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
            .with_context(|| format!("opening {}", path.display()))?;
        let reader = BufReader::new(file);

        let mut first_user_message = String::new();
        let mut directory = String::new();
        let mut messages: Vec<String> = Vec::new();
        let mut turn_count: u32 = 0;

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
                Err(_) => continue, // Skip malformed lines.
            };

            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

            // Extract working directory from first user message.
            if msg_type == "user" && directory.is_empty() {
                if let Some(cwd) = data.get("cwd").and_then(|v| v.as_str()) {
                    directory = cwd.to_string();
                }
            }

            // Process user messages.
            if msg_type == "user" {
                let msg = data.get("message").and_then(|v| v.as_object());
                let content = msg.and_then(|m| m.get("content")).cloned().unwrap_or(Value::Null);
                let is_meta = data.get("isMeta").and_then(|v| v.as_bool()).unwrap_or(false);

                let mut is_human_input = false;

                match &content {
                    Value::String(s) => {
                        is_human_input = true;
                        if !is_meta
                            && !s.starts_with("<command")
                            && !s.starts_with("<local-command")
                        {
                            messages.push(format!("» {}", s));
                            if first_user_message.is_empty() && s.len() > 10 {
                                first_user_message = s.clone();
                            }
                        }
                    }
                    Value::Array(parts) => {
                        // Determine if human input by checking first part type.
                        if let Some(first) = parts.first() {
                            if let Some(part_type) = first.get("type").and_then(|v| v.as_str()) {
                                if part_type == "text" {
                                    is_human_input = true;
                                }
                                // tool_result → automatic, not human input
                            }
                        }

                        for part in parts {
                            match part {
                                Value::Object(obj)
                                    if obj.get("type").and_then(|v| v.as_str()) == Some("text") =>
                                {
                                    if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                                        messages.push(format!("» {}", text));
                                        if first_user_message.is_empty() {
                                            first_user_message = text.to_string();
                                        }
                                    }
                                }
                                Value::String(s) => {
                                    messages.push(format!("» {}", s));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }

                if is_human_input {
                    turn_count += 1;
                }
            }

            // Extract assistant content.
            if msg_type == "assistant" {
                let msg = data.get("message").and_then(|v| v.as_object());
                let content = msg.and_then(|m| m.get("content")).cloned().unwrap_or(Value::Null);
                let mut has_text = false;

                match &content {
                    Value::String(s) if !s.is_empty() => {
                        messages.push(format!("  {}", s));
                        has_text = true;
                    }
                    Value::Array(parts) => {
                        for part in parts {
                            match part {
                                Value::Object(obj)
                                    if obj.get("type").and_then(|v| v.as_str()) == Some("text") =>
                                {
                                    if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                                        if !text.is_empty() {
                                            messages.push(format!("  {}", text));
                                            has_text = true;
                                        }
                                    }
                                }
                                Value::String(s) => {
                                    messages.push(format!("  {}", s));
                                    has_text = true;
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }

                if has_text {
                    turn_count += 1;
                }
            }
        }

        // Skip sessions with no actual user message.
        if first_user_message.is_empty() {
            return Ok(None);
        }

        // Skip sessions with no actual conversation content.
        if messages.is_empty() {
            return Ok(None);
        }

        let title = truncate_title(&first_user_message, 100);
        let content = messages.join("\n\n");
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

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

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn color(&self) -> &'static str {
        "#E87B35"
    }

    fn badge(&self) -> &'static str {
        "claude"
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
        let mut cmd = vec!["claude".to_string()];
        if yolo {
            cmd.push("--dangerously-skip-permissions".to_string());
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
