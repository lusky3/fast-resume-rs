/// OpenCode session adapter — SQLite backend.
///
/// Ported from python/fast_resume/adapters/opencode.py.
///
/// Two backends exist in the Python source:
/// - SQLite (`opencode.db`) — current OpenCode 1.2+ format — **implemented here**.
/// - Legacy split-JSON (`~/.local/share/opencode/storage/`) — TODO Phase 4+.
///
/// The SQLite schema:
/// - `session` table: id, title, directory, time_created, time_updated (ms)
/// - `message` table: id, session_id, time_created, data (JSON with `role`)
/// - `part`    table: message_id, session_id, time_created, data (JSON with `type`, `text`)
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::adapters::{AgentAdapter, ErrorCb, IncrementalResult, SessionCb};
use crate::config;
use crate::session::{ParseError, RawAdapterStats, Session};

pub struct OpenCodeAdapter {
    db_path: PathBuf,
    data_dir: PathBuf,
}

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self {
            db_path: config::opencode_db(),
            data_dir: config::opencode_dir(),
        }
    }

    /// Construct with a custom db path (for tests).
    pub fn with_db(db_path: PathBuf) -> Self {
        let data_dir = db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Self { db_path, data_dir }
    }

    fn load_sessions(&self, on_error: Option<ErrorCb<'_>>) -> Vec<Session> {
        match self.try_load_sessions() {
            Ok(sessions) => sessions,
            Err(e) => {
                let err = ParseError {
                    agent: self.name().to_string(),
                    file_path: self.db_path.display().to_string(),
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

    fn try_load_sessions(&self) -> Result<Vec<Session>> {
        let conn = rusqlite::Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;

        // Fetch all session rows.
        let mut stmt = conn.prepare(
            "SELECT id, title, directory, time_created, time_updated
             FROM session
             ORDER BY time_updated DESC",
        )?;

        struct SessionRow {
            id: String,
            title: String,
            directory: String,
            time_created: i64,
            time_updated: i64,
        }

        let session_rows: Vec<SessionRow> = stmt
            .query_map([], |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    directory: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    time_created: row.get(3).unwrap_or(0),
                    time_updated: row.get(4).unwrap_or(0),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        if session_rows.is_empty() {
            return Ok(Vec::new());
        }

        // Fetch message role data for all sessions in chunks to avoid SQLite
        // variable limit (~999 per query).
        let session_ids: Vec<&str> = session_rows.iter().map(|r| r.id.as_str()).collect();

        // messages_by_session: session_id → Vec<(message_id, role)>
        let mut messages_by_session: HashMap<String, Vec<(String, String)>> = HashMap::new();
        // parts_by_message: message_id → Vec<text>
        let mut parts_by_message: HashMap<String, Vec<String>> = HashMap::new();

        const CHUNK: usize = 900;

        for chunk in session_ids.chunks(CHUNK) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");

            // Fetch message roles via json_extract.
            let msg_sql = format!(
                "SELECT id, session_id, json_extract(data, '$.role')
                 FROM message
                 WHERE session_id IN ({placeholders})
                 ORDER BY time_created ASC"
            );
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
            let mut msg_stmt = conn.prepare(&msg_sql)?;
            let msg_rows = msg_stmt
                .query_map(params.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    ))
                })?
                .filter_map(|r| r.ok());

            for (msg_id, session_id, role) in msg_rows {
                messages_by_session
                    .entry(session_id)
                    .or_default()
                    .push((msg_id, role));
            }

            // Fetch text parts via json_extract.
            let part_sql = format!(
                "SELECT message_id, json_extract(data, '$.text')
                 FROM part
                 WHERE session_id IN ({placeholders})
                   AND json_extract(data, '$.type') = 'text'
                 ORDER BY time_created ASC"
            );
            let params2: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
            let mut part_stmt = conn.prepare(&part_sql)?;
            let part_rows = part_stmt
                .query_map(params2.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                })?
                .filter_map(|r| r.ok());

            for (msg_id, text) in part_rows {
                if !text.is_empty() {
                    parts_by_message.entry(msg_id).or_default().push(text);
                }
            }
        }

        // Build Session objects.
        let mut sessions = Vec::new();
        for row in &session_rows {
            if let Some(session) =
                build_session(row.id.as_str(), row.title.as_str(), row.directory.as_str(), row.time_created, row.time_updated, &messages_by_session, &parts_by_message, self.name())
            {
                sessions.push(session);
            }
        }

        Ok(sessions)
    }
}

impl Default for OpenCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_session(
    session_id: &str,
    title: &str,
    directory: &str,
    time_created: i64,
    time_updated: i64,
    messages_by_session: &HashMap<String, Vec<(String, String)>>,
    parts_by_message: &HashMap<String, Vec<String>>,
    agent_name: &str,
) -> Option<Session> {
    // Timestamps are integer milliseconds.
    let time_ms = if time_updated != 0 {
        time_updated
    } else {
        time_created
    };
    let ts_secs = if time_ms != 0 {
        time_ms as f64 / 1000.0
    } else {
        0.0
    };

    let timestamp = f64_to_timestamp(ts_secs);

    let session_msgs = messages_by_session.get(session_id).map(|v| v.as_slice()).unwrap_or(&[]);

    let mut messages: Vec<String> = Vec::new();
    for (msg_id, role) in session_msgs {
        let role_prefix = if role == "user" { "» " } else { "  " };
        let texts = parts_by_message.get(msg_id).map(|v| v.as_slice()).unwrap_or(&[]);
        for text in texts {
            messages.push(format!("{}{}", role_prefix, text));
        }
    }

    let final_title = if title.trim().is_empty() {
        "Untitled session".to_string()
    } else {
        title.to_string()
    };

    Some(Session {
        id: session_id.to_string(),
        agent: agent_name.to_string(),
        title: final_title,
        directory: directory.to_string(),
        timestamp,
        content: messages.join("\n\n"),
        message_count: session_msgs.len() as u32,
        mtime: ts_secs,
        yolo: false,
    })
}

fn f64_to_timestamp(secs: f64) -> jiff::Timestamp {
    let whole = secs as i64;
    let nanos = ((secs - whole as f64) * 1_000_000_000.0) as i32;
    jiff::Timestamp::new(whole, nanos).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

impl AgentAdapter for OpenCodeAdapter {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn color(&self) -> &'static str {
        "#CFCECD"
    }

    fn badge(&self) -> &'static str {
        "opencode"
    }

    fn supports_yolo(&self) -> bool {
        false
    }

    fn is_available(&self) -> bool {
        // SQLite backend only for Phase 4.
        self.db_path.exists()
    }

    fn find_sessions(&self) -> Vec<Session> {
        if !self.is_available() {
            return Vec::new();
        }
        self.load_sessions(None)
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

        // For incremental, we load all sessions (DB rows are cheap), then
        // emit only those newer than the stored mtime.
        let all_sessions = self.load_sessions(on_error);

        let mut all_current_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut new_or_modified = Vec::new();

        for session in all_sessions {
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

    fn get_resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
        vec![
            "opencode".to_string(),
            session.directory.clone(),
            "--session".to_string(),
            session.id.clone(),
        ]
    }

    fn get_raw_stats(&self) -> RawAdapterStats {
        let data_dir = self.data_dir.display().to_string();
        if !self.is_available() {
            return RawAdapterStats {
                agent: self.name().to_string(),
                data_dir,
                available: false,
                file_count: 0,
                total_bytes: 0,
            };
        }

        let total_bytes = self
            .db_path
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);

        RawAdapterStats {
            agent: self.name().to_string(),
            data_dir,
            available: true,
            file_count: 1,
            total_bytes,
        }
    }
}
