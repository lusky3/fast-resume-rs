//! Integration tests for ClaudeAdapter, mirroring tests/test_claude_adapter.py.
//! Uses in-memory fixtures (same shape as the Python tests) rather than hitting
//! ~/.claude on the test host.

use std::collections::HashMap;
use std::io::Write;
use tempfile::TempDir;

use fr::adapters::AgentAdapter;

fn write_jsonl(dir: &std::path::Path, name: &str, lines: &[serde_json::Value]) {
    let path = dir.join(name);
    let mut f = std::fs::File::create(path).unwrap();
    for v in lines {
        writeln!(f, "{}", serde_json::to_string(v).unwrap()).unwrap();
    }
}

/// Build a minimal Claude project dir with one session file.
fn temp_project_with_session(
    parent: &std::path::Path,
    project_name: &str,
    session_name: &str,
    lines: &[serde_json::Value],
) -> std::path::PathBuf {
    let project_dir = parent.join(project_name);
    std::fs::create_dir_all(&project_dir).unwrap();
    write_jsonl(&project_dir, session_name, lines);
    project_dir
}

fn basic_lines() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "user",
            "cwd": "/home/user/project",
            "message": {"content": "Help me fix this bug in the login system"}
        }),
        serde_json::json!({
            "type": "assistant",
            "message": {"content": "I'll help you fix the bug. Let me look at the code."}
        }),
        serde_json::json!({
            "type": "user",
            "message": {"content": "The error is in the validate function"}
        }),
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "Found it. The validate function has a null check issue."}]}
        }),
    ]
}

#[test]
fn test_parse_session_basic() {
    let tmp = TempDir::new().unwrap();
    let project = temp_project_with_session(
        tmp.path(),
        "my-project",
        "session-abc123.jsonl",
        &basic_lines(),
    );

    let adapter = fr::adapters::claude::ClaudeAdapter::with_dir(tmp.path().to_path_buf());
    let session_path = project.join("session-abc123.jsonl");
    let session = adapter.parse_session_file(&session_path, None).unwrap();

    assert_eq!(session.agent, "claude");
    assert_eq!(session.id, "session-abc123");
    assert_eq!(session.directory, "/home/user/project");
    assert!(session.title.contains("bug"), "title should mention 'bug'");
    assert!(session.content.contains("Help me fix"), "content should include user message");
    assert!(session.content.contains("I'll help"), "content should include assistant reply");
    assert!(session.message_count >= 2);
    assert!(!session.yolo);
}

#[test]
fn test_find_sessions_counts() {
    let tmp = TempDir::new().unwrap();
    temp_project_with_session(tmp.path(), "project-a", "session-001.jsonl", &basic_lines());
    temp_project_with_session(tmp.path(), "project-b", "session-002.jsonl", &basic_lines());

    let adapter = fr::adapters::claude::ClaudeAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();
    assert_eq!(sessions.len(), 2);
}

#[test]
fn test_agent_files_are_skipped() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    // A real session.
    write_jsonl(&project_dir, "session-real.jsonl", &basic_lines());
    // An agent sub-process file — must be ignored.
    write_jsonl(&project_dir, "agent-subagent.jsonl", &basic_lines());

    let adapter = fr::adapters::claude::ClaudeAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();
    assert_eq!(sessions.len(), 1, "agent- files should be skipped");
    assert_eq!(sessions[0].id, "session-real");
}

#[test]
fn test_incremental_no_change() {
    let tmp = TempDir::new().unwrap();
    temp_project_with_session(tmp.path(), "proj", "session-inc.jsonl", &basic_lines());

    let adapter = fr::adapters::claude::ClaudeAdapter::with_dir(tmp.path().to_path_buf());
    // First pass: get sessions + mtimes.
    let sessions = adapter.find_sessions();
    assert_eq!(sessions.len(), 1);

    // Build known map from first pass.
    let known: HashMap<String, (f64, String)> = sessions
        .iter()
        .map(|s| (s.id.clone(), (s.mtime, s.agent.clone())))
        .collect();

    // Second pass with matching known: no changes expected.
    let result = adapter.find_sessions_incremental(&known, None, None);
    assert!(result.new_or_modified.is_empty(), "no files changed so no updates");
    assert!(result.deleted_ids.is_empty(), "no files deleted");
}

#[test]
fn test_incremental_detects_new_file() {
    let tmp = TempDir::new().unwrap();
    temp_project_with_session(tmp.path(), "proj", "session-a.jsonl", &basic_lines());

    let adapter = fr::adapters::claude::ClaudeAdapter::with_dir(tmp.path().to_path_buf());

    // Known map is empty — simulates a fresh index.
    let known = HashMap::new();
    let result = adapter.find_sessions_incremental(&known, None, None);
    assert_eq!(result.new_or_modified.len(), 1);
    assert!(result.deleted_ids.is_empty());
}

#[test]
fn test_incremental_detects_deleted_session() {
    let tmp = TempDir::new().unwrap();
    temp_project_with_session(tmp.path(), "proj", "session-gone.jsonl", &basic_lines());

    let adapter = fr::adapters::claude::ClaudeAdapter::with_dir(tmp.path().to_path_buf());
    let sessions = adapter.find_sessions();
    let mut known: HashMap<String, (f64, String)> = sessions
        .iter()
        .map(|s| (s.id.clone(), (s.mtime, s.agent.clone())))
        .collect();

    // Add a ghost entry (as if a file was indexed before but deleted from disk).
    known.insert("ghost-session".to_string(), (1_000_000.0, "claude".to_string()));

    let result = adapter.find_sessions_incremental(&known, None, None);
    assert!(result.deleted_ids.contains(&"ghost-session".to_string()));
}

#[test]
fn test_resume_command_normal() {
    let s = fr::session::Session {
        id: "abc-123".to_string(),
        agent: "claude".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = fr::adapters::claude::ClaudeAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(cmd, vec!["claude", "--resume", "abc-123"]);
}

#[test]
fn test_resume_command_yolo() {
    let s = fr::session::Session {
        id: "abc-123".to_string(),
        agent: "claude".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = fr::adapters::claude::ClaudeAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, true);
    assert_eq!(
        cmd,
        vec!["claude", "--dangerously-skip-permissions", "--resume", "abc-123"]
    );
}
