//! Integration tests for CrushAdapter.

use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

use fr::adapters::AgentAdapter;
use fr::adapters::crush::CrushAdapter;

/// Create a minimal `crush.db` SQLite database with the expected schema.
///
/// Schema mirrors what the Python adapter queries:
///   sessions(id, title, message_count, updated_at, created_at)
///   messages(id, session_id, role, parts, created_at)
fn create_crush_db(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id           TEXT PRIMARY KEY,
            title        TEXT,
            message_count INTEGER DEFAULT 0,
            updated_at   INTEGER,
            created_at   INTEGER
         );
         CREATE TABLE messages (
            id         TEXT PRIMARY KEY,
            session_id TEXT,
            role       TEXT,
            parts      TEXT,
            created_at INTEGER
         );",
    )
    .unwrap();
}

fn insert_session(conn: &rusqlite::Connection, id: &str, title: &str, updated_at: i64) {
    conn.execute(
        "INSERT INTO sessions (id, title, message_count, updated_at, created_at) VALUES (?1, ?2, 1, ?3, ?3)",
        rusqlite::params![id, title, updated_at],
    )
    .unwrap();
}

fn insert_message(
    conn: &rusqlite::Connection,
    id: &str,
    session_id: &str,
    role: &str,
    text: &str,
    created_at: i64,
) {
    // Crush parts format: [{"type": "text", "data": {"text": "..."}}]
    let parts = serde_json::json!([{"type": "text", "data": {"text": text}}]).to_string();
    conn.execute(
        "INSERT INTO messages (id, session_id, role, parts, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, session_id, role, parts, created_at],
    )
    .unwrap();
}

fn write_projects_json(projects_file: &Path, entries: &[(&str, &str)]) {
    // entries: (project_path, data_dir)
    let projects: Vec<serde_json::Value> = entries
        .iter()
        .map(|(path, data_dir)| {
            serde_json::json!({
                "path": path,
                "data_dir": data_dir,
            })
        })
        .collect();
    let json = serde_json::json!({"projects": projects});
    let mut f = std::fs::File::create(projects_file).unwrap();
    write!(f, "{}", json).unwrap();
}

#[test]
fn test_find_sessions_two_projects() {
    let tmp = TempDir::new().unwrap();
    let projects_file = tmp.path().join("projects.json");

    // Project 1 — two sessions.
    let proj1_dir = tmp.path().join("proj1");
    std::fs::create_dir_all(&proj1_dir).unwrap();
    let db1_path = proj1_dir.join("crush.db");
    create_crush_db(&db1_path);
    {
        let conn = rusqlite::Connection::open(&db1_path).unwrap();
        insert_session(&conn, "sess-1a", "First session", 1_700_000_000);
        insert_message(&conn, "m1", "sess-1a", "user", "Hello from session 1a", 1_700_000_001);
        insert_message(&conn, "m2", "sess-1a", "assistant", "Hi there!", 1_700_000_002);

        insert_session(&conn, "sess-1b", "Second session", 1_700_100_000);
        insert_message(&conn, "m3", "sess-1b", "user", "Another project question", 1_700_100_001);
    }

    // Project 2 — one session.
    let proj2_dir = tmp.path().join("proj2");
    std::fs::create_dir_all(&proj2_dir).unwrap();
    let db2_path = proj2_dir.join("crush.db");
    create_crush_db(&db2_path);
    {
        let conn = rusqlite::Connection::open(&db2_path).unwrap();
        insert_session(&conn, "sess-2a", "", 1_700_200_000); // no title — falls back to first message
        insert_message(&conn, "m4", "sess-2a", "user", "What is Rust?", 1_700_200_001);
        insert_message(&conn, "m5", "sess-2a", "assistant", "Rust is a systems lang.", 1_700_200_002);
    }

    write_projects_json(
        &projects_file,
        &[
            ("/home/user/proj1", proj1_dir.to_str().unwrap()),
            ("/home/user/proj2", proj2_dir.to_str().unwrap()),
        ],
    );

    let adapter = CrushAdapter::with_projects_file(projects_file);
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 3, "Expected 3 sessions across 2 projects");

    // All sessions come from this adapter.
    assert!(sessions.iter().all(|s| s.agent == "crush"));

    // Find session with no title — should fall back to first user message.
    let sess_2a = sessions.iter().find(|s| s.id == "sess-2a").unwrap();
    assert_eq!(sess_2a.title, "What is Rust?");
    assert_eq!(sess_2a.directory, "/home/user/proj2");

    // Session with an explicit title keeps it.
    let sess_1a = sessions.iter().find(|s| s.id == "sess-1a").unwrap();
    assert_eq!(sess_1a.title, "First session");
    assert_eq!(sess_1a.directory, "/home/user/proj1");

    // Content includes both user and assistant messages.
    assert!(sess_1a.content.contains("Hello from session 1a"));
    assert!(sess_1a.content.contains("Hi there!"));
    assert!(sess_1a.content.contains("» "), "User messages should be prefixed with »");
}

#[test]
fn test_is_available_false_when_no_projects_file() {
    let tmp = TempDir::new().unwrap();
    let adapter = CrushAdapter::with_projects_file(tmp.path().join("nonexistent.json"));
    assert!(!adapter.is_available());
    assert!(adapter.find_sessions().is_empty());
}

#[test]
fn test_is_available_true_when_projects_file_exists() {
    let tmp = TempDir::new().unwrap();
    let projects_file = tmp.path().join("projects.json");
    write_projects_json(&projects_file, &[]);
    let adapter = CrushAdapter::with_projects_file(projects_file);
    assert!(adapter.is_available());
}

#[test]
fn test_skips_missing_db() {
    let tmp = TempDir::new().unwrap();
    let projects_file = tmp.path().join("projects.json");
    // data_dir points somewhere that doesn't have crush.db.
    write_projects_json(&projects_file, &[("/home/user/ghost", "/nonexistent/path")]);
    let adapter = CrushAdapter::with_projects_file(projects_file);
    assert!(adapter.find_sessions().is_empty());
}

#[test]
fn test_session_with_millisecond_timestamp() {
    let tmp = TempDir::new().unwrap();
    let projects_file = tmp.path().join("projects.json");
    let proj_dir = tmp.path().join("proj");
    std::fs::create_dir_all(&proj_dir).unwrap();
    let db_path = proj_dir.join("crush.db");
    create_crush_db(&db_path);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Timestamp in milliseconds (> 1e11).
        let ts_ms: i64 = 1_700_000_000_000;
        insert_session(&conn, "ms-sess", "Millisecond test", ts_ms);
        insert_message(&conn, "m1", "ms-sess", "user", "test ms timestamps", 1_700_000_000_001);
    }

    write_projects_json(&projects_file, &[("/home/user/ms", proj_dir.to_str().unwrap())]);
    let adapter = CrushAdapter::with_projects_file(projects_file);
    let sessions = adapter.find_sessions();
    assert_eq!(sessions.len(), 1);
    // Timestamp should be approximately 2023 (not year ~55000 from raw ms as seconds).
    let ts = sessions[0].timestamp.as_second();
    assert!(ts > 1_600_000_000 && ts < 1_800_000_000, "Timestamp {ts} should be in 2023 range");
}

#[test]
fn test_resume_command() {
    let s = fr::session::Session {
        id: "crush-123".to_string(),
        agent: "crush".to_string(),
        title: "test".to_string(),
        directory: "/home/user/proj".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = CrushAdapter::with_projects_file(tmp.path().join("p.json"));
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(cmd, vec!["crush"]);
}

#[test]
fn test_incremental_detects_new_session() {
    let tmp = TempDir::new().unwrap();
    let projects_file = tmp.path().join("projects.json");
    let proj_dir = tmp.path().join("proj");
    std::fs::create_dir_all(&proj_dir).unwrap();
    let db_path = proj_dir.join("crush.db");
    create_crush_db(&db_path);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        insert_session(&conn, "old-sess", "Old session", 1_700_000_000);
        insert_message(&conn, "m1", "old-sess", "user", "old question", 1_700_000_001);

        insert_session(&conn, "new-sess", "New session", 1_700_100_000);
        insert_message(&conn, "m2", "new-sess", "user", "new question", 1_700_100_001);
    }

    write_projects_json(&projects_file, &[("/p", proj_dir.to_str().unwrap())]);
    let adapter = CrushAdapter::with_projects_file(projects_file);

    // Known: old-sess already indexed at its timestamp; new-sess not known.
    let mut known = std::collections::HashMap::new();
    known.insert(
        "old-sess".to_string(),
        (1_700_000_000.0_f64, "crush".to_string()),
    );

    let result = adapter.find_sessions_incremental(&known, None, None);

    assert_eq!(result.new_or_modified.len(), 1);
    assert_eq!(result.new_or_modified[0].id, "new-sess");
    assert!(result.deleted_ids.is_empty());
}
