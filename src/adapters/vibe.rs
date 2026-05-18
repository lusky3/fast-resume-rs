/// Vibe (Mistral) session adapter.
///
/// Faithfully ported from python/fast_resume/adapters/vibe.py.
/// Sessions live under ~/.vibe/logs/session/ as `session_*` directories,
/// each containing `meta.json` and `messages.jsonl`.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::adapters::{file_based_incremental, AgentAdapter, ErrorCb, IncrementalResult, SessionCb};
use crate::config;
use crate::session::{ParseError, RawAdapterStats, Session};

fn system_time_to_f64(t: std::time::SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Parse an ISO-8601 / RFC-3339 string to a jiff Timestamp.
fn parse_iso_timestamp(s: &str, fallback_mtime: f64) -> jiff::Timestamp {
    // Normalise trailing 'Z' to '+00:00'.
    let s = if let Some(without_z) = s.strip_suffix('Z') {
        format!("{}+00:00", without_z)
    } else {
        s.to_string()
    };
    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
        return ts;
    }
    // Fall back to file mtime.
    let whole = fallback_mtime as i64;
    let nanos = ((fallback_mtime - whole as f64) * 1_000_000_000.0) as i32;
    jiff::Timestamp::new(whole, nanos).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
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

pub struct VibeAdapter {
    sessions_dir: PathBuf,
}

impl VibeAdapter {
    pub fn new() -> Self {
        Self {
            sessions_dir: config::vibe_dir(),
        }
    }

    pub fn with_dir(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// Scan: use meta.json mtime for change detection.
    fn scan(&self) -> Vec<(String, PathBuf, f64)> {
        let mut results = Vec::new();

        let read_dir = match std::fs::read_dir(&self.sessions_dir) {
            Ok(rd) => rd,
            Err(_) => return results,
        };

        for entry in read_dir.flatten() {
            let session_dir = entry.path();
            if !session_dir.is_dir() {
                continue;
            }
            let dir_name = session_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if !dir_name.starts_with("session_") {
                continue;
            }

            let meta_file = session_dir.join("meta.json");
            if !meta_file.exists() {
                continue;
            }

            // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
            let meta_mtime = match meta_file.metadata().and_then(|m| m.modified()) {
                Ok(t) => system_time_to_f64(t),
                Err(_) => continue,
            };

            // Use the later of meta.json or messages.jsonl mtime.
            let messages_file = session_dir.join("messages.jsonl");
            let mtime = if messages_file.exists() {
                let msg_mtime = messages_file
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(system_time_to_f64)
                    .unwrap_or(0.0);
                meta_mtime.max(msg_mtime)
            } else {
                meta_mtime
            };

            // Extract session_id from meta.json.
            let session_id = if let Ok(content) = std::fs::read_to_string(&meta_file) {
                serde_json::from_str::<Value>(&content)
                    .ok()
                    .and_then(|v| {
                        v.get("session_id")
                            .and_then(|id| id.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| dir_name.to_string())
            } else {
                dir_name.to_string()
            };

            // Use the session directory as the "path" passed to parse_session_file.
            results.push((session_id, session_dir, mtime));
        }

        results
    }

    pub fn parse_session_file(
        &self,
        path: &Path,
        on_error: Option<ErrorCb<'_>>,
    ) -> Option<Session> {
        match self.parse_session_dir(path) {
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

    fn parse_session_dir(&self, session_dir: &Path) -> anyhow::Result<Option<Session>> {
        let meta_file = session_dir.join("meta.json");
        let messages_file = session_dir.join("messages.jsonl");

        if !meta_file.exists() {
            return Ok(None);
        }

        // path is from adapter's own scan; not user-supplied.
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let meta_content = std::fs::read_to_string(&meta_file)?;
        let metadata: Value = serde_json::from_str(&meta_content)?;

        let meta_mtime = meta_file
            .metadata()
            .and_then(|m| m.modified())
            .map(system_time_to_f64)
            .unwrap_or(0.0);

        let session_id = metadata
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                session_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
            })
            .to_string();

        let directory = metadata
            .get("environment")
            .and_then(|e| e.get("working_directory"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let yolo = metadata
            .get("config")
            .and_then(|c| c.get("auto_approve"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || metadata
                .get("auto_approve")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

        let start_time_str = metadata
            .get("start_time")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let timestamp = if !start_time_str.is_empty() {
            parse_iso_timestamp(&start_time_str, meta_mtime)
        } else {
            f64_to_timestamp(meta_mtime)
        };

        let title_from_meta = metadata
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        // Parse messages.jsonl
        let mut messages: Vec<String> = Vec::new();
        let mut first_user_content = String::new();

        if messages_file.exists() {
            // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
            let file = std::fs::File::open(&messages_file)?;
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
                let msg: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if role == "system" {
                    continue;
                }
                let role_prefix = if role == "user" { "» " } else { "  " };
                let content = msg.get("content").cloned().unwrap_or(Value::Null);

                match content {
                    Value::String(s) if !s.is_empty() => {
                        if role == "user" && first_user_content.is_empty() {
                            first_user_content = s.clone();
                        }
                        messages.push(format!("{}{}", role_prefix, s));
                    }
                    Value::Array(parts) => {
                        for part in parts {
                            if let Value::Object(obj) = &part {
                                let text = obj.get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !text.is_empty() {
                                    if role == "user" && first_user_content.is_empty() {
                                        first_user_content = text.to_string();
                                    }
                                    messages.push(format!("{}{}", role_prefix, text));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let title = if !title_from_meta.is_empty() {
            title_from_meta
        } else if !first_user_content.is_empty() {
            truncate_title(&first_user_content, 80)
        } else {
            "Vibe session".to_string()
        };

        let mtime = meta_mtime.max(
            messages_file
                .metadata()
                .and_then(|m| m.modified())
                .map(system_time_to_f64)
                .unwrap_or(0.0),
        );

        Ok(Some(Session {
            id: session_id,
            agent: self.name().to_string(),
            title,
            directory,
            timestamp,
            content: messages.join("\n\n"),
            message_count: messages.len() as u32,
            mtime,
            yolo,
        }))
    }
}

impl Default for VibeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for VibeAdapter {
    fn name(&self) -> &'static str {
        "vibe"
    }

    fn color(&self) -> &'static str {
        "#FF6B35"
    }

    fn badge(&self) -> &'static str {
        "vibe"
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
        let mut cmd = vec!["vibe".to_string()];
        if yolo {
            cmd.push("--agent".to_string());
            cmd.push("auto-approve".to_string());
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
        // Count bytes of both meta.json and messages.jsonl per session.
        let total_bytes: u64 = files
            .iter()
            .map(|(_, session_dir, _)| {
                let meta_bytes = session_dir
                    .join("meta.json")
                    .metadata()
                    .map(|m| m.len())
                    .unwrap_or(0);
                let msg_bytes = session_dir
                    .join("messages.jsonl")
                    .metadata()
                    .map(|m| m.len())
                    .unwrap_or(0);
                meta_bytes + msg_bytes
            })
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
