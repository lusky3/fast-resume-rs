//! Integration tests for KiroAdapter.

use std::io::Write;
use tempfile::TempDir;

use fr::adapters::AgentAdapter;
use fr::adapters::kiro::KiroAdapter;

/// Write a Kiro session: <id>.json metadata + <id>.jsonl events.
fn write_kiro_session(
    sessions_dir: &std::path::Path,
    session_id: &str,
    cwd: &str,
    title: &str,
    prompts: &[&str],
    assistant_texts: &[&str],
) {
    let meta = serde_json::json!({
        "session_id": session_id,
        "cwd": cwd,
        "created_at": "2026-04-28T02:49:18.496286644Z",
        "updated_at": "2026-04-28T02:49:27.063836082Z",
        "title": title
    });
    let meta_path = sessions_dir.join(format!("{}.json", session_id));
    let mut f = std::fs::File::create(&meta_path).unwrap();
    writeln!(f, "{}", serde_json::to_string(&meta).unwrap()).unwrap();

    let events_path = sessions_dir.join(format!("{}.jsonl", session_id));
    let mut f = std::fs::File::create(events_path).unwrap();

    for prompt in prompts {
        let line = serde_json::json!({
            "version": "v1",
            "kind": "Prompt",
            "data": {
                "message_id": "m1",
                "content": [{"kind": "text", "data": prompt}]
            }
        });
        writeln!(f, "{}", serde_json::to_string(&line).unwrap()).unwrap();
    }

    for text in assistant_texts {
        let line = serde_json::json!({
            "version": "v1",
            "kind": "AssistantMessage",
            "data": {
                "message_id": "m2",
                "content": [
                    {"kind": "text", "data": text},
                    {"kind": "toolUse", "data": {"name": "write_file"}}
                ]
            }
        });
        writeln!(f, "{}", serde_json::to_string(&line).unwrap()).unwrap();
    }
}

/// Parse a basic Kiro session from uuid.json + uuid.jsonl pair.
#[test]
fn test_parse_session_basic() {
    let tmp = TempDir::new().unwrap();

    write_kiro_session(
        tmp.path(),
        "abc-123",
        "/home/user/project",
        "Write a hello world program",
        &["Write a hello world program"],
        &["Here is a hello world in Rust."],
    );

    let adapter = KiroAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.agent, "kiro");
    assert_eq!(s.id, "abc-123");
    assert_eq!(s.directory, "/home/user/project");
    // Title comes from meta.json.
    assert_eq!(s.title, "Write a hello world program");
    // Content has prompt text.
    assert!(s.content.contains("Write a hello world program"));
    // Content has assistant text but not tool-use.
    assert!(s.content.contains("Here is a hello world"));
    assert!(!s.content.contains("write_file"), "ToolResults should be excluded");
}

/// Without a .jsonl file, session is still parsed from meta.json alone.
#[test]
fn test_parse_session_no_events_file() {
    let tmp = TempDir::new().unwrap();

    let meta = serde_json::json!({
        "session_id": "meta-only",
        "cwd": "/tmp",
        "title": "Meta only session"
    });
    let meta_path = tmp.path().join("meta-only.json");
    let mut f = std::fs::File::create(&meta_path).unwrap();
    writeln!(f, "{}", serde_json::to_string(&meta).unwrap()).unwrap();

    let adapter = KiroAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();

    // Session with no events and no user prompts — empty title falls back to "Kiro session".
    // Whether it gets indexed depends on our minimum content check.
    // For Kiro, we always return a session (title comes from meta).
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "meta-only");
    assert_eq!(sessions[0].title, "Meta only session");
}

/// Resume command without yolo.
#[test]
fn test_resume_command_normal() {
    let s = fr::session::Session {
        id: "kiro-sess-abc".to_string(),
        agent: "kiro".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = KiroAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(cmd, vec!["kiro-cli", "chat", "--resume-id", "kiro-sess-abc"]);
}

/// Resume command with yolo inserts --trust-all-tools after "chat".
#[test]
fn test_resume_command_yolo() {
    let s = fr::session::Session {
        id: "kiro-sess-abc".to_string(),
        agent: "kiro".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = KiroAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, true);
    assert_eq!(
        cmd,
        vec![
            "kiro-cli",
            "chat",
            "--trust-all-tools",
            "--resume-id",
            "kiro-sess-abc"
        ]
    );
}
