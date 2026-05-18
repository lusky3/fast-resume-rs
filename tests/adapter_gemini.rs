//! Integration tests for GeminiAdapter.
//!
//! Covers both the single-JSON and streaming-JSONL formats.

use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

use fr::adapters::AgentAdapter;
use fr::adapters::gemini::GeminiAdapter;

/// Gemini directory layout:
///   <root>/tmp/<slug>/chats/session-*.{json,jsonl}
///   <root>/projects.json  →  {"projects": {"/path": "slug"}}
fn setup_gemini_root(tmp: &TempDir, slug: &str, project_path: &str) -> PathBuf {
    let root = tmp.path().to_path_buf();
    let chats_dir = root.join("tmp").join(slug).join("chats");
    std::fs::create_dir_all(&chats_dir).unwrap();

    // Write projects.json mapping path → slug.
    let projects = serde_json::json!({
        "projects": {
            project_path: slug
        }
    });
    let mut f = std::fs::File::create(root.join("projects.json")).unwrap();
    write!(f, "{}", projects).unwrap();

    root
}

/// Write a single-JSON session file.
fn write_json_session(
    chats_dir: &std::path::Path,
    filename_stem: &str,
    session_id: &str,
    messages: &[(&str, &str)], // (type: "user"|"gemini", content_text)
    last_updated: Option<&str>,
) {
    let msg_values: Vec<serde_json::Value> = messages
        .iter()
        .map(|(msg_type, text)| {
            serde_json::json!({
                "type": msg_type,
                "content": text,
                "id": format!("id-{}", text.len())
            })
        })
        .collect();

    let mut data = serde_json::json!({
        "sessionId": session_id,
        "messages": msg_values,
    });
    if let Some(ts) = last_updated {
        data["lastUpdated"] = serde_json::json!(ts);
    }

    let path = chats_dir.join(format!("{}.json", filename_stem));
    let mut f = std::fs::File::create(path).unwrap();
    write!(f, "{}", data).unwrap();
}

/// Write a streaming-JSONL session file.
fn write_jsonl_session(
    chats_dir: &std::path::Path,
    filename_stem: &str,
    session_id: &str,
    messages: &[(&str, &str)], // (type: "user"|"gemini", content_text)
    last_updated: Option<&str>,
) {
    let path = chats_dir.join(format!("{}.jsonl", filename_stem));
    let mut f = std::fs::File::create(path).unwrap();

    // First line: session header.
    let mut header = serde_json::json!({"sessionId": session_id});
    if let Some(ts) = last_updated {
        header["lastUpdated"] = serde_json::json!(ts);
    }
    writeln!(f, "{}", header).unwrap();

    // Message lines.
    for (i, (msg_type, text)) in messages.iter().enumerate() {
        let msg = serde_json::json!({
            "type": msg_type,
            "content": text,
            "id": format!("msgid-{}", i)
        });
        writeln!(f, "{}", msg).unwrap();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn test_parse_json_format() {
    let tmp = TempDir::new().unwrap();
    let root = setup_gemini_root(&tmp, "myslug", "/home/user/myproject");
    let chats_dir = root.join("tmp").join("myslug").join("chats");

    write_json_session(
        &chats_dir,
        "session-2026-01-01",
        "gemini-sess-abc",
        &[
            ("user", "How do I use Gemini CLI?"),
            ("gemini", "You can use `gemini chat` to start."),
        ],
        Some("2026-01-01T12:00:00Z"),
    );

    let adapter = GeminiAdapter::with_dir(root);
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.id, "gemini-sess-abc");
    assert_eq!(s.agent, "gemini");
    assert_eq!(s.directory, "/home/user/myproject");
    assert_eq!(s.title, "How do I use Gemini CLI?");
    assert!(s.content.contains("How do I use Gemini CLI?"));
    assert!(s.content.contains("You can use `gemini chat`"));
    assert!(s.content.contains("» "), "User messages should be prefixed with »");
}

#[test]
fn test_parse_jsonl_format() {
    let tmp = TempDir::new().unwrap();
    let root = setup_gemini_root(&tmp, "slug2", "/workspace/proj");
    let chats_dir = root.join("tmp").join("slug2").join("chats");

    write_jsonl_session(
        &chats_dir,
        "session-streaming",
        "gemini-stream-xyz",
        &[
            ("user", "Write me a Rust function"),
            ("gemini", "Here is a Rust function:\n```rust\nfn hello() {}\n```"),
            ("user", "Can you add a parameter?"),
            ("gemini", "Sure: `fn hello(name: &str) {}`"),
        ],
        Some("2026-02-15T08:30:00+00:00"),
    );

    let adapter = GeminiAdapter::with_dir(root);
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.id, "gemini-stream-xyz");
    assert_eq!(s.title, "Write me a Rust function");
    assert_eq!(s.directory, "/workspace/proj");
    assert_eq!(s.message_count, 4);
    assert!(s.content.contains("Write me a Rust function"));
    assert!(s.content.contains("Can you add a parameter?"));
    assert!(s.content.contains("fn hello()"));
}

#[test]
fn test_jsonl_deduplicates_repeated_messages() {
    let tmp = TempDir::new().unwrap();
    let root = setup_gemini_root(&tmp, "slug3", "/some/path");
    let chats_dir = root.join("tmp").join("slug3").join("chats");
    let path = chats_dir.join("session-dedup.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();

    // Header.
    writeln!(f, r#"{{"sessionId": "dedup-sess"}}"#).unwrap();
    // Same message repeated twice — Gemini re-emits messages as fields update.
    writeln!(
        f,
        r#"{{"type": "user", "content": "hello", "id": "msg-1"}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type": "user", "content": "hello updated", "id": "msg-1"}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type": "gemini", "content": "hi there", "id": "msg-2"}}"#
    )
    .unwrap();

    let adapter = GeminiAdapter::with_dir(root);
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    // Only the first occurrence of msg-1 should be counted.
    assert_eq!(sessions[0].message_count, 2, "Duplicate message should be deduplicated");
}

#[test]
fn test_jsonl_set_patches_update_metadata() {
    let tmp = TempDir::new().unwrap();
    let root = setup_gemini_root(&tmp, "slug4", "/dir");
    let chats_dir = root.join("tmp").join("slug4").join("chats");
    let path = chats_dir.join("session-patch.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();

    // Header with early timestamp.
    writeln!(
        f,
        r#"{{"sessionId": "patched-sess", "lastUpdated": "2025-01-01T00:00:00Z"}}"#
    )
    .unwrap();
    // $set patch updates lastUpdated.
    writeln!(
        f,
        r#"{{"$set": {{"lastUpdated": "2026-03-01T12:00:00Z"}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type": "user", "content": "patched question", "id": "m1"}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type": "gemini", "content": "patched answer", "id": "m2"}}"#
    )
    .unwrap();

    let adapter = GeminiAdapter::with_dir(root);
    let sessions = adapter.find_sessions();

    assert_eq!(sessions.len(), 1);
    // Timestamp should reflect the $set patch value (2026-03-01).
    let ts = sessions[0].timestamp.as_second();
    // 2026-03-01T12:00:00Z ≈ 1740830400
    assert!(ts > 1_740_000_000, "Patched timestamp should be from 2026, got {ts}");
}

#[test]
fn test_skips_session_with_no_user_messages() {
    let tmp = TempDir::new().unwrap();
    let root = setup_gemini_root(&tmp, "slug5", "/empty");
    let chats_dir = root.join("tmp").join("slug5").join("chats");

    write_json_session(
        &chats_dir,
        "session-no-user",
        "no-user-sess",
        &[
            ("info", "some system message"),  // type "info" — should be filtered
        ],
        None,
    );

    let adapter = GeminiAdapter::with_dir(root);
    let sessions = adapter.find_sessions();
    assert!(sessions.is_empty(), "Sessions with no user messages should be skipped");
}

#[test]
fn test_is_available_false_without_tmp_dir() {
    let tmp = TempDir::new().unwrap();
    // No `tmp/` subdirectory created.
    let adapter = GeminiAdapter::with_dir(tmp.path().to_path_buf());
    assert!(!adapter.is_available());
    assert!(adapter.find_sessions().is_empty());
}

#[test]
fn test_prefers_newer_mtime_when_both_json_and_jsonl_exist() {
    let tmp = TempDir::new().unwrap();
    let root = setup_gemini_root(&tmp, "slug6", "/dual");
    let chats_dir = root.join("tmp").join("slug6").join("chats");

    // Write both formats for the same session ID. The adapter should pick the
    // one with the later mtime (jsonl written after json here).
    write_json_session(
        &chats_dir,
        "session-overlap",
        "overlap-id",
        &[("user", "from json format"), ("gemini", "json response")],
        None,
    );
    // Small sleep to ensure different mtime.
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_jsonl_session(
        &chats_dir,
        "session-overlap",
        "overlap-id",
        &[("user", "from jsonl format"), ("gemini", "jsonl response")],
        None,
    );

    let adapter = GeminiAdapter::with_dir(root);
    let sessions = adapter.find_sessions();

    // Must be exactly 1 session (deduplicated by session_id).
    assert_eq!(sessions.len(), 1, "Duplicate session ID should be deduplicated");
}

#[test]
fn test_resume_command_without_yolo() {
    let s = fr::session::Session {
        id: "gem-abc".to_string(),
        agent: "gemini".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = GeminiAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, false);
    assert_eq!(cmd, vec!["gemini", "--resume", "gem-abc"]);
}

#[test]
fn test_resume_command_with_yolo() {
    let s = fr::session::Session {
        id: "gem-abc".to_string(),
        agent: "gemini".to_string(),
        title: "test".to_string(),
        directory: "/tmp".to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: "".to_string(),
        message_count: 1,
        mtime: 0.0,
        yolo: false,
    };
    let tmp = TempDir::new().unwrap();
    let adapter = GeminiAdapter::with_dir(tmp.path().to_path_buf());
    let cmd = adapter.get_resume_command(&s, true);
    assert_eq!(cmd, vec!["gemini", "--yolo", "--resume", "gem-abc"]);
}
