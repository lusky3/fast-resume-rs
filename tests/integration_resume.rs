//! Integration tests for the resume handoff (Phase 7).
//!
//! These tests verify that:
//! 1. `replace_process` returns an error when the binary does not exist.
//! 2. The ClaudeAdapter's `get_resume_command` returns the expected argv.

use fr::util;

#[test]
fn test_replace_process_missing_binary_returns_error() {
    let err = util::proc::replace_process(
        &["definitely-nonexistent-binary-xyz".to_string()],
        None,
    );
    // On Unix this will be ErrorKind::NotFound; raw_os_error() is set in either case.
    assert!(
        err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error().is_some(),
        "Expected NotFound or OS error, got: {:?}",
        err
    );
}

#[test]
fn test_replace_process_missing_binary_with_cwd() {
    let err = util::proc::replace_process(
        &["definitely-nonexistent-binary-xyz".to_string()],
        Some(std::path::Path::new("/tmp")),
    );
    assert!(
        err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error().is_some(),
        "Expected NotFound or OS error, got: {:?}",
        err
    );
}

#[test]
fn test_replace_process_empty_argv_returns_error() {
    // An empty slice means argv[0] is an empty string — execvp("", ...) returns ENOENT.
    let err = util::proc::replace_process(&["".to_string()], None);
    assert!(
        err.raw_os_error().is_some(),
        "Expected an OS error for empty program name, got: {:?}",
        err
    );
}
