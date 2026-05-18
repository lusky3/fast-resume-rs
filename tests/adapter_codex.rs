//! Integration tests for CodexAdapter.

use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

use fr::adapters::AgentAdapter;
use fr::adapters::codex::CodexAdapter;

fn write_jsonl(path: &PathBuf, lines: &[serde_json::Value]) {
    let mut f = std::fs::File::create(path).unwrap();
    for v in lines {
        writeln!(f, "{}", serde_json::to_string(v).unwrap()).unwrap();
    }
}

fn basic_codex_lines(session_id: &str, cwd: &str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "session_meta",
            "payload": {"id": session_id, "cwd": cwd}
        }),
        serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "Help me refactor this function"
            }
        }),
        serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "agent_reasoning",
                "text": "I'll analyze the function structure."
            }
        }),
        serde_json::json!({
            "type": "response_item",
            "payload": {
                "role": "assistant",
                "content": [{"text": "Here's the refactored code."}]
            }
        }),
    ]
}

/// Parse a basic Codex session from a date-nested subdirectory.
#[test]
fn test_parse_session_basic() {
    let tmp = TempDir::new().unwrap();
    let session_dir = tmp.path().join("2025").join("12").join("20");
    std::fs::create_dir_all(&session_dir).unwrap();

    let session_file = session_dir.join("rollout-2025-12-20T10-00-00-abc123.jsonl");
    write_jsonl(&session_file, &basic_codex_lines("abc123", "/home/user/project"));

    let adapter = CodexAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.agent, "codex");
    assert_eq!(s.id, "abc123");
    assert_eq!(s.directory, "/home/user/project");
    assert!(!s.title.is_empty());
    assert!(s.content.contains("Help me refactor"));
}

/// Yolo flag is detected from turn_context events.
#[test]
fn test_parse_yolo_detection() {
    let tmp = TempDir::new().unwrap();
    let session_dir = tmp.path().join("2025").join("12").join("20");
    std::fs::create_dir_all(&session_dir).unwrap();

    let mut lines = basic_codex_lines("yolo-sess", "/tmp");
    lines.push(serde_json::json!({
        "type": "turn_context",
        "payload": {
            "approval_policy": "never",
            "sandbox_policy": {}
        }
    }));

    let session_file = session_dir.join("rollout-yolo.jsonl");
    write_jsonl(&session_file, &lines);

    let adapter = CodexAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].yolo, "yolo should be true when approval_policy is 'never'");
}

/// Resume command without yolo.
#[test]
fn test_resume_command_normal() {
    let s = fr::session::Session {
        id: "sess-123".to_string(),
        agent: "codex".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = CodexAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(cmd, vec!["codex", "resume", "sess-123"]);
}

/// Resume command with yolo prepends the bypass flag.
#[test]
fn test_resume_command_yolo() {
    let s = fr::session::Session {
        id: "sess-123".to_string(),
        agent: "codex".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: true,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = CodexAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, true);
    assert_eq!(
        cmd,
        vec![
            "codex",
            "--dangerously-bypass-approvals-and-sandbox",
            "resume",
            "sess-123"
        ]
    );
}
