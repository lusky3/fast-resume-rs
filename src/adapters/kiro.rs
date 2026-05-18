/// Kiro CLI session adapter.
///
/// Faithfully ported from python/fast_resume/adapters/kiro.py.
/// Sessions live under ~/.kiro/sessions/cli/ as <uuid>.json + <uuid>.jsonl pairs.
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

fn f64_to_timestamp(secs: f64) -> jiff::Timestamp {
    let whole = secs as i64;
    let nanos = ((secs - whole as f64) * 1_000_000_000.0) as i32;
    jiff::Timestamp::new(whole, nanos).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

/// Parse an ISO-8601 / RFC-3339 string to a jiff Timestamp.
fn parse_iso_timestamp(s: &str, fallback_mtime: f64) -> jiff::Timestamp {
    let s = if let Some(without_z) = s.strip_suffix('Z') {
        format!("{}+00:00", without_z)
    } else {
        s.to_string()
    };
    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
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

/// Collect plain-text segments from a Kiro content array.
fn extract_text(content: &Value) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if let Value::Array(items) = content {
        for item in items {
            if let Value::Object(obj) = item {
                if obj.get("kind").and_then(|v| v.as_str()) != Some("text") {
                    continue;
                }
                if let Some(text) = obj.get("data").and_then(|v| v.as_str()) {
                    let text = text.trim();
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
            }
        }
    }
    parts.join("\n")
}

pub struct KiroAdapter {
    sessions_dir: PathBuf,
}

impl KiroAdapter {
    pub fn new() -> Self {
        Self {
            sessions_dir: config::kiro_dir(),
        }
    }

    pub fn with_dir(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    fn scan(&self) -> Vec<(String, PathBuf, f64)> {
        let mut results = Vec::new();

        let read_dir = match std::fs::read_dir(&self.sessions_dir) {
            Ok(rd) => rd,
            Err(_) => return results,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if !path.is_file() {
                continue;
            }

            let meta_mtime = match path.metadata().and_then(|m| m.modified()) {
                Ok(t) => system_time_to_f64(t),
                Err(_) => continue,
            };

            // Use the later of .json or .jsonl mtime.
            let events_file = path.with_extension("jsonl");
            let mtime = if events_file.exists() {
                let ev_mtime = events_file
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(system_time_to_f64)
                    .unwrap_or(0.0);
                meta_mtime.max(ev_mtime)
            } else {
                meta_mtime
            };

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            results.push((stem, path, mtime));
        }

        results
    }

    pub fn parse_session_file(
        &self,
        path: &Path,
        on_error: Option<ErrorCb<'_>>,
    ) -> Option<Session> {
        match self.parse_meta_file(path) {
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

    fn parse_meta_file(&self, meta_file: &Path) -> anyhow::Result<Option<Session>> {
        let meta_mtime = meta_file
            .metadata()
            .and_then(|m| m.modified())
            .map(system_time_to_f64)
            .unwrap_or(0.0);

        // meta_file comes from adapter's own scan; not user-supplied.
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let meta_content = std::fs::read_to_string(meta_file)?;
        let meta: Value = serde_json::from_str(&meta_content)?;

        let session_id = meta
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                meta_file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
            })
            .to_string();

        let directory = meta.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let title_from_meta = meta
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let ts_str = meta
            .get("updated_at")
            .or_else(|| meta.get("created_at"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let timestamp = if !ts_str.is_empty() {
            parse_iso_timestamp(ts_str, meta_mtime)
        } else {
            f64_to_timestamp(meta_mtime)
        };

        // Parse the .jsonl events file.
        let events_file = meta_file.with_extension("jsonl");
        let mut messages: Vec<String> = Vec::new();
        let mut first_user_prompt = String::new();
        let mut turn_count: u32 = 0;

        if events_file.exists() {
            // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
            let file = std::fs::File::open(&events_file)?;
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

                let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let data = entry.get("data").cloned().unwrap_or(Value::Null);

                if kind == "Prompt" {
                    let text = extract_text(data.get("content").unwrap_or(&Value::Array(vec![])));
                    if !text.is_empty() {
                        messages.push(format!("» {}", text));
                        turn_count += 1;
                        if first_user_prompt.is_empty() {
                            first_user_prompt = text;
                        }
                    }
                } else if kind == "AssistantMessage" {
                    let text = extract_text(data.get("content").unwrap_or(&Value::Array(vec![])));
                    if !text.is_empty() {
                        messages.push(format!("  {}", text));
                        turn_count += 1;
                    }
                }
                // ToolResults are intentionally excluded.
            }
        }

        let raw_title = if !title_from_meta.is_empty() {
            title_from_meta
        } else if !first_user_prompt.is_empty() {
            first_user_prompt
        } else {
            "Kiro session".to_string()
        };
        let title = truncate_title(&raw_title, 80);

        let events_mtime = events_file
            .metadata()
            .and_then(|m| m.modified())
            .map(system_time_to_f64)
            .unwrap_or(0.0);
        let mtime = meta_mtime.max(events_mtime);

        Ok(Some(Session {
            id: session_id,
            agent: self.name().to_string(),
            title,
            directory,
            timestamp,
            content: messages.join("\n\n"),
            message_count: turn_count,
            mtime,
            yolo: false,
        }))
    }
}

impl Default for KiroAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for KiroAdapter {
    fn name(&self) -> &'static str {
        "kiro"
    }

    fn color(&self) -> &'static str {
        "#5C1FFB"
    }

    fn badge(&self) -> &'static str {
        "kiro"
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
        let mut cmd = vec!["kiro-cli".to_string(), "chat".to_string()];
        if yolo {
            cmd.push("--trust-all-tools".to_string());
        }
        cmd.push("--resume-id".to_string());
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
            .map(|(_, meta_path, _)| {
                let meta_bytes = meta_path.metadata().map(|m| m.len()).unwrap_or(0);
                let ev_bytes = meta_path
                    .with_extension("jsonl")
                    .metadata()
                    .map(|m| m.len())
                    .unwrap_or(0);
                meta_bytes + ev_bytes
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
