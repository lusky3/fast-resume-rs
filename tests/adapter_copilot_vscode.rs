//! Integration tests for CopilotVSCodeAdapter.

use std::io::Write;
use tempfile::TempDir;

use fr::adapters::AgentAdapter;
use fr::adapters::copilot_vscode::CopilotVSCodeAdapter;

/// Write a VS Code Copilot Chat session JSON file.
///
/// Format mirrors the real VS Code Copilot Chat storage format.
fn write_vscode_session(
    chat_dir: &std::path::Path,
    session_id: &str,
    custom_title: &str,
    requests: &[(&str, &str)],  // (user_text, assistant_text)
    last_message_date_ms: Option<u64>,
) {
    let req_values: Vec<serde_json::Value> = requests
        .iter()
        .map(|(user, assistant)| {
            serde_json::json!({
                "message": {"text": user},
                "response": [{"value": assistant}],
                "contentReferences": []
            })
        })
        .collect();

    let mut data = serde_json::json!({
        "sessionId": session_id,
        "requests": req_values,
    });

    if !custom_title.is_empty() {
        data["customTitle"] = serde_json::json!(custom_title);
    }
    if let Some(ts) = last_message_date_ms {
        data["lastMessageDate"] = serde_json::json!(ts);
    }

    let path = chat_dir.join(format!("{}.json", session_id));
    let mut f = std::fs::File::create(path).unwrap();
    write!(f, "{}", data).unwrap();
}

/// Write a workspace.json with a file:// folder URI.
fn write_workspace_json(ws_dir: &std::path::Path, folder_path: &str) {
    let data = serde_json::json!({
        "folder": format!("file://{}", folder_path)
    });
    let path = ws_dir.join("workspace.json");
    let mut f = std::fs::File::create(path).unwrap();
    write!(f, "{}", data).unwrap();
}

#[test]
fn test_find_session_from_empty_window_sessions() {
    let tmp = TempDir::new().unwrap();

    // Empty-window chat sessions directory.
    let chat_dir = tmp.path().join("emptyWindowChatSessions");
    std::fs::create_dir_all(&chat_dir).unwrap();

    write_vscode_session(
        &chat_dir,
        "vscode-sess-1",
        "",
        &[
            ("How do I write a for loop in Rust?", "Use `for i in 0..10 {}`"),
            ("What about iterators?", "Use `.iter()` and `.map()`"),
        ],
        Some(1_700_000_000_000),
    );

    // Empty workspace storage (no workspace sessions).
    let ws_storage = tmp.path().join("workspaceStorage");
    std::fs::create_dir_all(&ws_storage).unwrap();

    let adapter = CopilotVSCodeAdapter::with_dirs(chat_dir, ws_storage);
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.id, "vscode-sess-1");
    assert_eq!(s.agent, "copilot-vscode");
    // Title falls back to first user message (no customTitle).
    assert_eq!(s.title, "How do I write a for loop in Rust?");
    // Content includes both turns.
    assert!(s.content.contains("How do I write a for loop in Rust?"));
    assert!(s.content.contains("Use `.iter()` and `.map()`"));
    assert!(s.content.contains("» "), "User messages should have » prefix");
    // Timestamp from lastMessageDate.
    assert_eq!(s.timestamp.as_second(), 1_700_000_000);
}

#[test]
fn test_find_session_from_workspace_storage() {
    let tmp = TempDir::new().unwrap();

    let chat_sessions_dir = tmp.path().join("emptyWindowChatSessions");
    std::fs::create_dir_all(&chat_sessions_dir).unwrap();

    // Workspace storage: one workspace hash directory.
    let ws_storage = tmp.path().join("workspaceStorage");
    let ws_dir = ws_storage.join("abc123hash");
    let chat_dir = ws_dir.join("chatSessions");
    std::fs::create_dir_all(&chat_dir).unwrap();

    write_workspace_json(&ws_dir, "/home/user/myproject");
    write_vscode_session(
        &chat_dir,
        "ws-sess-1",
        "My Custom Title",
        &[("Tell me about async Rust", "async/await in Rust uses Future...")],
        None,
    );

    let adapter = CopilotVSCodeAdapter::with_dirs(chat_sessions_dir, ws_storage);
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.id, "ws-sess-1");
    assert_eq!(s.title, "My Custom Title"); // customTitle set
    assert_eq!(s.directory, "/home/user/myproject");
    assert!(s.content.contains("Tell me about async Rust"));
    assert!(s.content.contains("async/await in Rust uses Future"));
}

#[test]
fn test_is_available_false_when_no_sessions() {
    let tmp = TempDir::new().unwrap();
    let chat_dir = tmp.path().join("empty_chat");
    let ws_dir = tmp.path().join("empty_ws");
    std::fs::create_dir_all(&chat_dir).unwrap();
    std::fs::create_dir_all(&ws_dir).unwrap();

    let adapter = CopilotVSCodeAdapter::with_dirs(chat_dir, ws_dir);
    assert!(!adapter.is_available());
    assert!(adapter.find_sessions().is_empty());
}

#[test]
fn test_is_available_true_with_session_file() {
    let tmp = TempDir::new().unwrap();
    let chat_dir = tmp.path().join("chat");
    let ws_dir = tmp.path().join("ws");
    std::fs::create_dir_all(&chat_dir).unwrap();
    std::fs::create_dir_all(&ws_dir).unwrap();

    write_vscode_session(
        &chat_dir,
        "any-session",
        "",
        &[("hello", "hi")],
        None,
    );

    let adapter = CopilotVSCodeAdapter::with_dirs(chat_dir, ws_dir);
    assert!(adapter.is_available());
}

#[test]
fn test_skips_session_with_empty_requests() {
    let tmp = TempDir::new().unwrap();
    let chat_dir = tmp.path().join("chat");
    let ws_dir = tmp.path().join("ws");
    std::fs::create_dir_all(&chat_dir).unwrap();
    std::fs::create_dir_all(&ws_dir).unwrap();

    // Session with no requests array.
    let data = serde_json::json!({
        "sessionId": "empty-requests",
        "requests": []
    });
    let path = chat_dir.join("empty-requests.json");
    let mut f = std::fs::File::create(path).unwrap();
    write!(f, "{}", data).unwrap();

    let adapter = CopilotVSCodeAdapter::with_dirs(chat_dir, ws_dir);
    let sessions = adapter.find_sessions();
    assert!(sessions.is_empty(), "Sessions with no requests should be skipped");
}

#[test]
fn test_multiple_workspace_sessions() {
    let tmp = TempDir::new().unwrap();
    let chat_dir = tmp.path().join("chat");
    let ws_storage = tmp.path().join("ws");
    std::fs::create_dir_all(&chat_dir).unwrap();

    // Two workspace directories, one session each.
    for (hash, proj, session_id) in &[
        ("hash1", "/proj/alpha", "sess-alpha"),
        ("hash2", "/proj/beta", "sess-beta"),
    ] {
        let ws_dir = ws_storage.join(hash);
        let c_dir = ws_dir.join("chatSessions");
        std::fs::create_dir_all(&c_dir).unwrap();
        write_workspace_json(&ws_dir, proj);
        write_vscode_session(
            &c_dir,
            session_id,
            "",
            &[("question", "answer")],
            None,
        );
    }

    let adapter = CopilotVSCodeAdapter::with_dirs(chat_dir, ws_storage);
    let sessions = adapter.find_sessions();
    assert_eq!(sessions.len(), 2);

    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"sess-alpha"));
    assert!(ids.contains(&"sess-beta"));
}

#[test]
fn test_resume_command_with_directory() {
    let s = fr::session::Session {
        id: "vs-123".to_string(),
        agent: "copilot-vscode".to_string(),
        title: "test".to_string(),
        directory: "/home/user/project".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = CopilotVSCodeAdapter::with_dirs(
        tmp.path().join("c"),
        tmp.path().join("w"),
    );
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(cmd, vec!["code", "/home/user/project"]);
}

#[test]
fn test_resume_command_without_directory() {
    let s = fr::session::Session {
        id: "vs-456".to_string(),
        agent: "copilot-vscode".to_string(),
        title: "test".to_string(),
        directory: String::new(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = CopilotVSCodeAdapter::with_dirs(
        tmp.path().join("c"),
        tmp.path().join("w"),
    );
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(cmd, vec!["code"]);
}
