/// VS Code Copilot Chat session adapter.
///
/// Ported from python/fast_resume/adapters/copilot_vscode.py.
///
/// VS Code stores chat sessions in two locations:
///   1. Empty-window sessions: `<vscode_config>/User/globalStorage/emptyWindowChatSessions/*.json`
///   2. Workspace sessions:    `<vscode_config>/User/workspaceStorage/<hash>/chatSessions/*.json`
///
/// The workspace directory is recovered from `workspaceStorage/<hash>/workspace.json`
/// which holds a `file://` URI to the folder.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::adapters::{AgentAdapter, ErrorCb, IncrementalResult, SessionCb, MTIME_TOLERANCE};
use crate::config;
use crate::session::{ParseError, RawAdapterStats, Session};

pub struct CopilotVSCodeAdapter {
    chat_sessions_dir: PathBuf,
    workspace_storage_dir: PathBuf,
}

impl CopilotVSCodeAdapter {
    pub fn new() -> Self {
        Self {
            chat_sessions_dir: config::copilot_vscode_chat_sessions_dir(),
            workspace_storage_dir: config::copilot_vscode_workspace_storage_dir(),
        }
    }

    /// Construct with custom directories (for tests).
    pub fn with_dirs(chat_sessions_dir: PathBuf, workspace_storage_dir: PathBuf) -> Self {
        Self {
            chat_sessions_dir,
            workspace_storage_dir,
        }
    }

    /// Enumerate all session files: `(session_file, workspace_directory)`.
    fn get_all_session_files(&self) -> Vec<(PathBuf, String)> {
        let mut result: Vec<(PathBuf, String)> = Vec::new();

        // Empty-window sessions (no workspace directory).
        if self.chat_sessions_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&self.chat_sessions_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("json") {
                        result.push((path, String::new()));
                    }
                }
            }
        }

        // Workspace-specific sessions.
        if self.workspace_storage_dir.exists() {
            if let Ok(ws_entries) = std::fs::read_dir(&self.workspace_storage_dir) {
                for ws_entry in ws_entries.flatten() {
                    let ws_dir = ws_entry.path();
                    if !ws_dir.is_dir() {
                        continue;
                    }
                    let chat_dir = ws_dir.join("chatSessions");
                    if !chat_dir.exists() {
                        continue;
                    }
                    let ws_directory = get_workspace_directory(&ws_dir);
                    if let Ok(chat_entries) = std::fs::read_dir(&chat_dir) {
                        for chat_entry in chat_entries.flatten() {
                            let path = chat_entry.path();
                            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                                result.push((path, ws_directory.clone()));
                            }
                        }
                    }
                }
            }
        }

        result
    }

    /// Extract the session ID from a session file without a full parse.
    fn get_session_id_from_file(&self, session_file: &Path) -> Option<String> {
        let content = std::fs::read(session_file).ok()?;
        let data: Value = serde_json::from_slice(&content).ok()?;
        Some(
            data.get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    session_file
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                })
                .to_string(),
        )
    }

    fn parse_session(
        &self,
        session_file: &Path,
        workspace_directory: &str,
        on_error: Option<ErrorCb<'_>>,
    ) -> Option<Session> {
        match self.try_parse_session(session_file, workspace_directory) {
            Ok(s) => s,
            Err(e) => {
                if let Some(cb) = on_error {
                    cb(ParseError {
                        agent: self.name().to_string(),
                        file_path: session_file.display().to_string(),
                        error_type: "ParseError".to_string(),
                        message: e.to_string(),
                    });
                }
                None
            }
        }
    }

    fn try_parse_session(
        &self,
        session_file: &Path,
        workspace_directory: &str,
    ) -> anyhow::Result<Option<Session>> {
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let content = std::fs::read(session_file)?;
        let data: Value = serde_json::from_slice(&content)?;

        let session_id = data
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                session_file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
            })
            .to_string();

        let custom_title = data
            .get("customTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let requests = match data.get("requests").and_then(|v| v.as_array()) {
            Some(r) if !r.is_empty() => r.clone(),
            _ => return Ok(None),
        };

        let mut messages: Vec<String> = Vec::new();
        let mut directory = workspace_directory.to_string();
        let mut turn_count: u32 = 0;

        for req in &requests {
            // User message.
            let user_text = req
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !user_text.is_empty() {
                messages.push(format!("» {}", user_text));
                turn_count += 1;
            }

            // Try to recover workspace directory from content references.
            if directory.is_empty() {
                if let Some(refs) = req.get("contentReferences").and_then(|v| v.as_array()) {
                    for r in refs {
                        if let Some(fs_path) = r
                            .get("reference")
                            .and_then(|rd| rd.get("uri"))
                            .and_then(|u| u.get("fsPath"))
                            .and_then(|v| v.as_str())
                        {
                            if !fs_path.is_empty() {
                                directory = Path::new(fs_path)
                                    .parent()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default();
                                break;
                            }
                        }
                    }
                }
            }

            // Assistant response parts.
            let mut has_response = false;
            if let Some(response) = req.get("response").and_then(|v| v.as_array()) {
                for part in response {
                    if let Some(value) = part.get("value").and_then(|v| v.as_str()) {
                        if !value.is_empty() {
                            messages.push(format!("  {}", value));
                            has_response = true;
                        }
                    }
                }
            }
            if has_response {
                turn_count += 1;
            }
        }

        if messages.is_empty() {
            return Ok(None);
        }

        // Build title: prefer customTitle, then first user message.
        let title = if !custom_title.is_empty() {
            custom_title
        } else {
            let first = messages[0].trim_start_matches("» ").trim();
            truncate_title(first, 100)
        };

        // Parse timestamp.
        let last_message_date = data.get("lastMessageDate").and_then(|v| v.as_f64());
        let creation_date = data.get("creationDate").and_then(|v| v.as_f64());
        let mtime_fallback = session_file
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let ts_secs = if let Some(ms) = last_message_date {
            ms / 1000.0
        } else if let Some(ms) = creation_date {
            ms / 1000.0
        } else {
            mtime_fallback
        };

        let timestamp = f64_to_timestamp(ts_secs);

        Ok(Some(Session {
            id: session_id,
            agent: self.name().to_string(),
            title,
            directory,
            timestamp,
            content: messages.join("\n\n"),
            message_count: turn_count,
            mtime: mtime_fallback,
            yolo: false,
        }))
    }
}

impl Default for CopilotVSCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Workspace directory helper ────────────────────────────────────────────────

/// Read the workspace folder path from `workspace.json` inside a workspace storage dir.
/// Returns an empty string on failure.
fn get_workspace_directory(workspace_dir: &Path) -> String {
    let workspace_json = workspace_dir.join("workspace.json");
    if !workspace_json.exists() {
        return String::new();
    }
    let content = match std::fs::read(&workspace_json) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let data: Value = match serde_json::from_slice(&content) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let folder = data
        .get("folder")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if folder.starts_with("file://") {
        // Decode percent-encoded path component.
        let path_part = folder.trim_start_matches("file://");
        // Simple percent-decoding for common cases (%20 → space, etc.)
        percent_decode(path_part)
    } else {
        folder.to_string()
    }
}

/// Minimal percent-decode for file URI paths.
/// Only decodes `%XX` sequences — sufficient for typical paths.
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                hex_val(bytes[i + 1]),
                hex_val(bytes[i + 2]),
            ) {
                result.push(char::from(h * 16 + l));
                i += 3;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ── Small utilities ───────────────────────────────────────────────────────────

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

// ── AgentAdapter implementation ───────────────────────────────────────────────

impl AgentAdapter for CopilotVSCodeAdapter {
    fn name(&self) -> &'static str {
        "copilot-vscode"
    }

    fn color(&self) -> &'static str {
        "#007ACC"
    }

    fn badge(&self) -> &'static str {
        "vscode"
    }

    fn supports_yolo(&self) -> bool {
        false
    }

    fn is_available(&self) -> bool {
        // Empty-window sessions directory has at least one JSON file.
        if self.chat_sessions_dir.exists() {
            if let Ok(mut entries) = std::fs::read_dir(&self.chat_sessions_dir) {
                if entries.any(|e| {
                    e.ok()
                        .map(|e| {
                            e.path().extension().and_then(|x| x.to_str()) == Some("json")
                        })
                        .unwrap_or(false)
                }) {
                    return true;
                }
            }
        }
        // Or workspace storage contains at least one chatSessions directory.
        if self.workspace_storage_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&self.workspace_storage_dir) {
                for entry in entries.flatten() {
                    let chat_dir = entry.path().join("chatSessions");
                    if chat_dir.exists() {
                        if let Ok(mut chat_entries) = std::fs::read_dir(&chat_dir) {
                            if chat_entries.any(|e| {
                                e.ok()
                                    .map(|e| {
                                        e.path().extension().and_then(|x| x.to_str())
                                            == Some("json")
                                    })
                                    .unwrap_or(false)
                            }) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    fn find_sessions(&self) -> Vec<Session> {
        if !self.is_available() {
            return Vec::new();
        }
        self.get_all_session_files()
            .into_iter()
            .filter_map(|(path, ws_dir)| self.parse_session(&path, &ws_dir, None))
            .collect()
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

        // Build current state: session_id → (path, mtime, ws_directory).
        let mut current: HashMap<String, (PathBuf, f64, String)> = HashMap::new();
        for (session_file, ws_directory) in self.get_all_session_files() {
            let session_id = match self.get_session_id_from_file(&session_file) {
                Some(id) => id,
                None => continue,
            };
            let mtime = match session_file.metadata().and_then(|m| m.modified()) {
                Ok(t) => t
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0),
                Err(_) => continue,
            };
            current.insert(session_id, (session_file, mtime, ws_directory));
        }

        let mut new_or_modified = Vec::new();
        for (session_id, (path, mtime, ws_dir)) in &current {
            let known_entry = known.get(session_id);
            if known_entry.is_none() || mtime > &(known_entry.unwrap().0 + MTIME_TOLERANCE) {
                if let Some(session) = self.parse_session(path, ws_dir, on_error) {
                    if let Some(cb) = on_session {
                        cb(session.clone());
                    }
                    new_or_modified.push(session);
                }
            }
        }

        let current_ids: std::collections::HashSet<&String> = current.keys().collect();
        let deleted_ids = known
            .iter()
            .filter(|(id, (_, agent))| agent == self.name() && !current_ids.contains(id))
            .map(|(id, _)| id.clone())
            .collect();

        IncrementalResult {
            new_or_modified,
            deleted_ids,
        }
    }

    fn get_resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
        if !session.directory.is_empty() {
            vec!["code".to_string(), session.directory.clone()]
        } else {
            vec!["code".to_string()]
        }
    }

    fn get_raw_stats(&self) -> RawAdapterStats {
        let data_dir = self.chat_sessions_dir.display().to_string();
        if !self.is_available() {
            return RawAdapterStats {
                agent: self.name().to_string(),
                data_dir,
                available: false,
                file_count: 0,
                total_bytes: 0,
            };
        }

        let files = self.get_all_session_files();
        let file_count = files.len() as u64;
        let total_bytes: u64 = files
            .iter()
            .map(|(path, _)| path.metadata().map(|m| m.len()).unwrap_or(0))
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
