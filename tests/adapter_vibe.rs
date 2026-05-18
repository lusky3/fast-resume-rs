//! Integration tests for VibeAdapter.

use std::io::Write;
use tempfile::TempDir;

use fr::adapters::AgentAdapter;
use fr::adapters::vibe::VibeAdapter;

fn write_vibe_session(
    sessions_dir: &std::path::Path,
    dir_name: &str,
    session_id: &str,
    title: &str,
    directory: &str,
    yolo: bool,
    messages: &[(&str, &str)], // (role, content)
) {
    let session_dir = sessions_dir.join(dir_name);
    std::fs::create_dir_all(&session_dir).unwrap();

    let meta = serde_json::json!({
        "session_id": session_id,
        "start_time": "2025-12-20T10:00:00Z",
        "environment": {"working_directory": directory},
        "title": title,
        "config": {"auto_approve": yolo}
    });
    let mut f = std::fs::File::create(session_dir.join("meta.json")).unwrap();
    writeln!(f, "{}", serde_json::to_string(&meta).unwrap()).unwrap();

    let mut f = std::fs::File::create(session_dir.join("messages.jsonl")).unwrap();
    for (role, content) in messages {
        let line = serde_json::json!({"role": role, "content": content});
        writeln!(f, "{}", serde_json::to_string(&line).unwrap()).unwrap();
    }
}

/// Parse a Vibe session folder with meta.json + messages.jsonl.
#[test]
fn test_parse_session_basic() {
    let tmp = TempDir::new().unwrap();

    write_vibe_session(
        tmp.path(),
        "session_20251220_100000_abc12345",
        "abc12345-full-session-id",
        "Build REST API",
        "/home/user/project",
        false,
        &[
            ("system", "You are a helpful assistant."),
            ("user", "Help me write a REST API"),
            ("assistant", "I'll help you create a REST API."),
        ],
    );

    let adapter = VibeAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.agent, "vibe");
    assert_eq!(s.id, "abc12345-full-session-id");
    assert_eq!(s.directory, "/home/user/project");
    assert_eq!(s.title, "Build REST API");
    assert!(s.content.contains("Help me write a REST API"));
    assert!(s.content.contains("I'll help you create"));
    // System messages should not appear in content.
    assert!(!s.content.contains("You are a helpful assistant"));
    assert!(!s.yolo);
}

/// Yolo flag from config.auto_approve is detected.
#[test]
fn test_parse_yolo_flag() {
    let tmp = TempDir::new().unwrap();

    write_vibe_session(
        tmp.path(),
        "session_yolo",
        "yolo-session-id",
        "Yolo session",
        "/tmp",
        true,
        &[("user", "Do something dangerous")],
    );

    let adapter = VibeAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].yolo, "yolo flag should be set from config.auto_approve");
}

/// Directories not prefixed with session_ are skipped.
#[test]
fn test_non_session_dirs_skipped() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("other_dir")).unwrap();

    write_vibe_session(
        tmp.path(),
        "session_valid",
        "valid-id",
        "Valid session",
        "/tmp",
        false,
        &[("user", "Hello")],
    );

    let adapter = VibeAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1, "only session_ dirs should be scanned");
}

/// Resume command without yolo.
#[test]
fn test_resume_command_normal() {
    let s = fr::session::Session {
        id: "vibe-sess-123".to_string(),
        agent: "vibe".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = VibeAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(cmd, vec!["vibe", "--resume", "vibe-sess-123"]);
}

/// Resume command with yolo includes agent flag.
#[test]
fn test_resume_command_yolo() {
    let s = fr::session::Session {
        id: "vibe-sess-123".to_string(),
        agent: "vibe".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: true,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = VibeAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, true);
    assert_eq!(
        cmd,
        vec!["vibe", "--agent", "auto-approve", "--resume", "vibe-sess-123"]
    );
}
