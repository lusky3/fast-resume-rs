/// Replace the current process with `argv`, after chdir-ing to `cwd`.
///
/// On Unix: uses `CommandExt::exec` which calls execvp(). Returns `std::io::Error` only
/// on failure (it does not return on success).
///
/// On non-Unix: spawns a child, waits, and exits with the child's exit code.
///
/// **The caller must call `ratatui::restore()` before this function.**
pub fn replace_process(argv: &[String], cwd: Option<&std::path::Path>) -> std::io::Error {
    replace_process_impl(argv, cwd)
}

#[cfg(unix)]
fn replace_process_impl(argv: &[String], cwd: Option<&std::path::Path>) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    // exec() only returns on failure — on success the current process is replaced.
    cmd.exec()
}

#[cfg(not(unix))]
fn replace_process_impl(argv: &[String], cwd: Option<&std::path::Path>) -> std::io::Error {
    // Windows: true POSIX-style process replacement does not exist. Spawn a child,
    // wait for it, and exit with its status code — best effort.
    use std::process::{Command, exit};
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    match cmd.status() {
        Ok(status) => exit(status.code().unwrap_or(1)),
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replace_process_missing_binary_returns_error() {
        let err = replace_process(
            &["definitely-nonexistent-binary-xyz".to_string()],
            None,
        );
        // Should get NotFound or some OS error indicating the binary was not found.
        assert!(
            err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error().is_some(),
            "Expected NotFound or OS error, got: {:?}",
            err
        );
    }

    #[test]
    fn test_replace_process_missing_binary_with_cwd_returns_error() {
        let err = replace_process(
            &["definitely-nonexistent-binary-xyz".to_string()],
            Some(std::path::Path::new("/tmp")),
        );
        assert!(
            err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error().is_some(),
            "Expected NotFound or OS error, got: {:?}",
            err
        );
    }
}
