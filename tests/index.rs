//! Integration tests for TantivyIndex.

use std::io::Write;
use tempfile::TempDir;

use fr::index::TantivyIndex;
use fr::session::Session;

fn make_session(id: &str, title: &str, agent: &str, directory: &str) -> Session {
    Session {
        id: id.to_string(),
        agent: agent.to_string(),
        title: title.to_string(),
        directory: directory.to_string(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: format!("Content for session {}", id),
        message_count: 2,
        mtime: 1_700_000_000.0,
        yolo: false,
    }
}

/// Round-trip: add a session, retrieve it, assert fields match.
#[test]
fn test_round_trip_single_session() {
    let tmp = TempDir::new().unwrap();
    let index = TantivyIndex::new(tmp.path().join("idx"));

    let s = make_session("sess-001", "Fix the login bug", "claude", "/home/user/project");
    index.update_sessions(&[s.clone()]).unwrap();

    let all = index.get_all_sessions().unwrap();
    assert_eq!(all.len(), 1);
    let retrieved = &all[0];
    assert_eq!(retrieved.id, "sess-001");
    assert_eq!(retrieved.title, "Fix the login bug");
    assert_eq!(retrieved.agent, "claude");
    assert_eq!(retrieved.directory, "/home/user/project");
}

/// Search by title term returns the matching session.
#[test]
fn test_search_by_title() {
    let tmp = TempDir::new().unwrap();
    let index = TantivyIndex::new(tmp.path().join("idx"));

    let s1 = make_session("sess-a", "Build REST API endpoint", "codex", "/home/user/api");
    let s2 = make_session("sess-b", "Fix authentication bug", "claude", "/home/user/auth");

    index.update_sessions(&[s1, s2]).unwrap();

    let results = index.search("REST API", 10).unwrap();
    assert!(!results.is_empty(), "should find at least one result");
    assert_eq!(results[0].0, "sess-a", "REST API session should rank first");
}

/// Schema-version bump: writing version 21 into the index dir causes a wipe
/// on the next `ensure_index`, so no pre-existing documents survive.
#[test]
fn test_schema_version_bump_wipes_index() {
    let tmp = TempDir::new().unwrap();
    let idx_dir = tmp.path().join("idx");
    std::fs::create_dir_all(&idx_dir).unwrap();

    // Write a stale version number.
    let version_file = idx_dir.join(".schema_version");
    let mut f = std::fs::File::create(&version_file).unwrap();
    write!(f, "21").unwrap();
    drop(f);

    // Create index — should detect version mismatch, wipe, and rebuild fresh.
    let index = TantivyIndex::new(idx_dir.clone());
    index.ensure_index().unwrap();

    // No documents should exist in the freshly-wiped index.
    let count = index.get_session_count().unwrap();
    assert_eq!(count, 0, "wiped index should be empty");

    // Version file should now contain the current schema version.
    let new_version: u32 = std::fs::read_to_string(&idx_dir.join(".schema_version"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(new_version, fr::config::SCHEMA_VERSION);
}

/// `get_known_sessions` returns the mtime we stored.
#[test]
fn test_get_known_sessions_returns_mtime() {
    let tmp = TempDir::new().unwrap();
    let index = TantivyIndex::new(tmp.path().join("idx"));

    let mut s = make_session("sess-mtime", "Test mtime", "claude", "/tmp");
    s.mtime = 1_750_000_123.456;

    index.update_sessions(&[s]).unwrap();

    let known = index.get_known_sessions().unwrap();
    assert!(known.contains_key("sess-mtime"));
    let (mtime, agent) = &known["sess-mtime"];
    assert!((*mtime - 1_750_000_123.456).abs() < 0.01, "mtime should round-trip");
    assert_eq!(agent, "claude");
}

/// `delete_sessions` removes documents from the index.
#[test]
fn test_delete_sessions() {
    let tmp = TempDir::new().unwrap();
    let index = TantivyIndex::new(tmp.path().join("idx"));

    let s1 = make_session("keep-me", "Keep this", "claude", "/tmp");
    let s2 = make_session("delete-me", "Delete this", "codex", "/tmp");
    index.update_sessions(&[s1, s2]).unwrap();

    assert_eq!(index.get_session_count().unwrap(), 2);

    index.delete_sessions(&["delete-me".to_string()]).unwrap();
    assert_eq!(index.get_session_count().unwrap(), 1);

    let known = index.get_known_sessions().unwrap();
    assert!(known.contains_key("keep-me"));
    assert!(!known.contains_key("delete-me"));
}
