//! Integration tests for OpenCodeAdapter (SQLite backend).

use std::path::Path;
use tempfile::TempDir;

use fr::adapters::AgentAdapter;
use fr::adapters::opencode::OpenCodeAdapter;

/// Create a minimal `opencode.db` with the schema the adapter expects.
///
/// Tables:
///   session(id, title, directory, time_created, time_updated)  — timestamps in ms
///   message(id, session_id, time_created, data)                — data has {"role": ...}
///   part(message_id, session_id, time_created, data)           — data has {"type":"text","text":...}
fn create_opencode_db(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (
            id           TEXT PRIMARY KEY,
            title        TEXT,
            directory    TEXT,
            time_created INTEGER,
            time_updated INTEGER
         );
         CREATE TABLE message (
            id           TEXT PRIMARY KEY,
            session_id   TEXT,
            time_created INTEGER,
            data         TEXT
         );
         CREATE TABLE part (
            id           TEXT PRIMARY KEY,
            message_id   TEXT,
            session_id   TEXT,
            time_created INTEGER,
            data         TEXT
         );",
    )
    .unwrap();
}

fn insert_opencode_session(
    conn: &rusqlite::Connection,
    id: &str,
    title: &str,
    directory: &str,
    time_created: i64,
    time_updated: i64,
) {
    conn.execute(
        "INSERT INTO session (id, title, directory, time_created, time_updated)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, title, directory, time_created, time_updated],
    )
    .unwrap();
}

fn insert_opencode_message(
    conn: &rusqlite::Connection,
    id: &str,
    session_id: &str,
    role: &str,
    time_created: i64,
) {
    let data = serde_json::json!({"role": role}).to_string();
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, session_id, time_created, data],
    )
    .unwrap();
}

fn insert_opencode_part(
    conn: &rusqlite::Connection,
    id: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
    time_created: i64,
) {
    let data = serde_json::json!({"type": "text", "text": text}).to_string();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, message_id, session_id, time_created, data],
    )
    .unwrap();
}

#[test]
fn test_find_session_basic() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("opencode.db");
    create_opencode_db(&db_path);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        insert_opencode_session(
            &conn,
            "oc-sess-1",
            "My OpenCode Session",
            "/home/user/project",
            1_700_000_000_000,
            1_700_100_000_000,
        );
        insert_opencode_message(&conn, "msg-1", "oc-sess-1", "user", 1_700_000_001_000);
        insert_opencode_part(&conn, "part-1", "msg-1", "oc-sess-1", "How do I use Rust?", 1_700_000_001_001);

        insert_opencode_message(&conn, "msg-2", "oc-sess-1", "assistant", 1_700_000_002_000);
        insert_opencode_part(&conn, "part-2", "msg-2", "oc-sess-1", "Rust is a systems language.", 1_700_000_002_001);
    }

    let adapter = OpenCodeAdapter::with_db(db_path);
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.id, "oc-sess-1");
    assert_eq!(s.agent, "opencode");
    assert_eq!(s.title, "My OpenCode Session");
    assert_eq!(s.directory, "/home/user/project");

    // Content contains both messages.
    assert!(s.content.contains("How do I use Rust?"), "content: {}", s.content);
    assert!(s.content.contains("Rust is a systems language."), "content: {}", s.content);
    assert!(s.content.contains("» "), "User messages should have » prefix");

    // Timestamp should reflect time_updated / 1000 = 1_700_100_000 seconds.
    assert_eq!(s.timestamp.as_second(), 1_700_100_000);
}

#[test]
fn test_find_sessions_empty_db() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("opencode.db");
    create_opencode_db(&db_path);

    let adapter = OpenCodeAdapter::with_db(db_path);
    let sessions = adapter.find_sessions();
    assert!(sessions.is_empty());
}

#[test]
fn test_is_available_false_when_no_db() {
    let tmp = TempDir::new().unwrap();
    let adapter = OpenCodeAdapter::with_db(tmp.path().join("nonexistent.db"));
    assert!(!adapter.is_available());
    assert!(adapter.find_sessions().is_empty());
}

#[test]
fn test_two_sessions() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("opencode.db");
    create_opencode_db(&db_path);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        insert_opencode_session(
            &conn,
            "sess-a",
            "Session A",
            "/proj/a",
            1_700_000_000_000,
            1_700_000_000_000,
        );
        insert_opencode_message(&conn, "m-a1", "sess-a", "user", 1_700_000_001_000);
        insert_opencode_part(&conn, "p-a1", "m-a1", "sess-a", "Question A", 1_700_000_001_001);

        insert_opencode_session(
            &conn,
            "sess-b",
            "Session B",
            "/proj/b",
            1_700_100_000_000,
            1_700_200_000_000,
        );
        insert_opencode_message(&conn, "m-b1", "sess-b", "user", 1_700_100_001_000);
        insert_opencode_part(&conn, "p-b1", "m-b1", "sess-b", "Question B", 1_700_100_001_001);
    }

    let adapter = OpenCodeAdapter::with_db(db_path);
    let sessions = adapter.find_sessions();
    assert_eq!(sessions.len(), 2);

    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"sess-a"));
    assert!(ids.contains(&"sess-b"));
}

#[test]
fn test_resume_command() {
    let s = fr::session::Session {
        id: "oc-123".to_string(),
        agent: "opencode".to_string(),
        title: "test".to_string(),
        directory: "/home/user/project".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = OpenCodeAdapter::with_db(tmp.path().join("oc.db"));
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(
        cmd,
        vec!["opencode", "/home/user/project", "--session", "oc-123"]
    );
}

#[test]
fn test_untitled_session_gets_default_title() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("opencode.db");
    create_opencode_db(&db_path);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        insert_opencode_session(&conn, "no-title", "", "/dir", 1_700_000_000_000, 1_700_000_000_000);
        insert_opencode_message(&conn, "m1", "no-title", "user", 1_700_000_001_000);
        insert_opencode_part(&conn, "p1", "m1", "no-title", "Some question", 1_700_000_001_001);
    }
    let adapter = OpenCodeAdapter::with_db(db_path);
    let sessions = adapter.find_sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title, "Untitled session");
}

#[test]
fn test_incremental_new_and_existing() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("opencode.db");
    create_opencode_db(&db_path);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        insert_opencode_session(&conn, "existing", "Existing", "/e", 1_700_000_000_000, 1_700_000_000_000);
        insert_opencode_message(&conn, "me1", "existing", "user", 1_700_000_001_000);
        insert_opencode_part(&conn, "pe1", "me1", "existing", "existing q", 1_700_000_001_001);

        insert_opencode_session(&conn, "brand-new", "New", "/n", 1_700_100_000_000, 1_700_100_000_000);
        insert_opencode_message(&conn, "mn1", "brand-new", "user", 1_700_100_001_000);
        insert_opencode_part(&conn, "pn1", "mn1", "brand-new", "new q", 1_700_100_001_001);
    }

    let adapter = OpenCodeAdapter::with_db(db_path);

    // existing is already known at its exact mtime.
    let mut known = std::collections::HashMap::new();
    known.insert(
        "existing".to_string(),
        (1_700_000_000.0_f64, "opencode".to_string()),
    );

    let result = adapter.find_sessions_incremental(&known, None, None);
    assert_eq!(result.new_or_modified.len(), 1);
    assert_eq!(result.new_or_modified[0].id, "brand-new");
    assert!(result.deleted_ids.is_empty());
}
