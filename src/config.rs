/// Configuration constants and path helpers.
///
/// Ported from python/fast_resume/config.py.
/// SCHEMA_VERSION is bumped to 22 for the Rust port (tokenizer changes may differ).
use std::collections::HashMap;
use std::path::PathBuf;

use directories::BaseDirs;

pub const SCHEMA_VERSION: u32 = 22;

/// Information about a single agent for display purposes.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub color: &'static str,
    pub badge: &'static str,
}

/// Returns the base cache directory: `~/.cache/fast-resume`.
pub fn cache_dir() -> PathBuf {
    BaseDirs::new()
        .map(|b| b.cache_dir().join("fast-resume"))
        .unwrap_or_else(|| PathBuf::from(".cache/fast-resume"))
}

/// Returns the Tantivy index directory: `~/.cache/fast-resume/tantivy_index`.
pub fn index_dir() -> PathBuf {
    cache_dir().join("tantivy_index")
}

/// Returns the parse-error log path: `~/.cache/fast-resume/parse-errors.log`.
pub fn log_file() -> PathBuf {
    cache_dir().join("parse-errors.log")
}

fn home() -> PathBuf {
    BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("~"))
}

pub fn claude_dir() -> PathBuf {
    home().join(".claude").join("projects")
}

pub fn codex_dir() -> PathBuf {
    home().join(".codex").join("sessions")
}

pub fn opencode_dir() -> PathBuf {
    // $XDG_DATA_HOME/opencode or ~/.local/share/opencode
    BaseDirs::new()
        .map(|b| b.data_dir().join("opencode"))
        .unwrap_or_else(|| home().join(".local").join("share").join("opencode"))
}

pub fn opencode_legacy_dir() -> PathBuf {
    opencode_dir().join("storage")
}

pub fn opencode_db() -> PathBuf {
    opencode_dir().join("opencode.db")
}

pub fn vibe_dir() -> PathBuf {
    home().join(".vibe").join("logs").join("session")
}

pub fn crush_projects_file() -> PathBuf {
    // ~/.local/share/crush/projects.json
    BaseDirs::new()
        .map(|b| b.data_dir().join("crush").join("projects.json"))
        .unwrap_or_else(|| {
            home()
                .join(".local")
                .join("share")
                .join("crush")
                .join("projects.json")
        })
}

pub fn copilot_dir() -> PathBuf {
    home().join(".copilot").join("session-state")
}

pub fn gemini_dir() -> PathBuf {
    home().join(".gemini")
}

/// Returns the VS Code configuration root (platform-specific).
/// - Linux:  ~/.config/Code
/// - macOS:  ~/Library/Application Support/Code
/// - Windows: %APPDATA%/Code
pub fn vscode_config_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home()
            .join("Library")
            .join("Application Support")
            .join("Code")
    }
    #[cfg(target_os = "windows")]
    {
        BaseDirs::new()
            .map(|b| b.config_dir().join("Code"))
            .unwrap_or_else(|| home().join("AppData").join("Roaming").join("Code"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home().join(".config").join("Code")
    }
}

/// Empty-window Copilot Chat sessions: ~/.config/Code/User/globalStorage/emptyWindowChatSessions
pub fn copilot_vscode_chat_sessions_dir() -> PathBuf {
    vscode_config_dir()
        .join("User")
        .join("globalStorage")
        .join("emptyWindowChatSessions")
}

/// Workspace storage root: ~/.config/Code/User/workspaceStorage
pub fn copilot_vscode_workspace_storage_dir() -> PathBuf {
    vscode_config_dir().join("User").join("workspaceStorage")
}

pub fn kiro_dir() -> PathBuf {
    home().join(".kiro").join("sessions").join("cli")
}

/// Map of agent name → display info. Hex colors match the Python config.
pub fn agents() -> HashMap<&'static str, AgentInfo> {
    let mut m = HashMap::new();
    m.insert(
        "claude",
        AgentInfo {
            color: "#E87B35",
            badge: "claude",
        },
    );
    m.insert(
        "codex",
        AgentInfo {
            color: "#00A67E",
            badge: "codex",
        },
    );
    m.insert(
        "opencode",
        AgentInfo {
            color: "#CFCECD",
            badge: "opencode",
        },
    );
    m.insert(
        "vibe",
        AgentInfo {
            color: "#FF6B35",
            badge: "vibe",
        },
    );
    m.insert(
        "crush",
        AgentInfo {
            color: "#6B51FF",
            badge: "crush",
        },
    );
    m.insert(
        "copilot-cli",
        AgentInfo {
            color: "#9CA3AF",
            badge: "copilot",
        },
    );
    m.insert(
        "copilot-vscode",
        AgentInfo {
            color: "#007ACC",
            badge: "vscode",
        },
    );
    m.insert(
        "gemini",
        AgentInfo {
            color: "#4285F4",
            badge: "gemini",
        },
    );
    m.insert(
        "kiro",
        AgentInfo {
            color: "#5C1FFB",
            badge: "kiro",
        },
    );
    m
}
