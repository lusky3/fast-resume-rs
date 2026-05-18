use ratatui::style::{Color, Modifier, Style};

// Agent brand colours ported from config.py / Python tui/styles.py
// Each constant is the primary hex colour converted to RGB.

pub const USER_MSG_STYLE: Style = Style::new()
    .fg(Color::Cyan)
    .add_modifier(Modifier::BOLD);

pub const PREVIEW_BORDER: Style = Style::new().fg(Color::DarkGray);

pub const SEARCH_BORDER_ACTIVE: Style = Style::new().fg(Color::Cyan);

pub const SEARCH_BORDER_INACTIVE: Style = Style::new().fg(Color::DarkGray);

pub const SELECTED_ROW: Style = Style::new().add_modifier(Modifier::REVERSED);

/// Return the brand colour for a given agent slug.
pub fn agent_color(agent: &str) -> Color {
    match agent {
        // Claude — Anthropic brand orange
        "claude" => Color::Rgb(210, 115, 55),
        // Codex — OpenAI green
        "codex" => Color::Rgb(16, 163, 127),
        // Copilot CLI / VS Code — GitHub blue-gray
        "copilot-cli" | "copilot-vscode" => Color::Rgb(30, 130, 216),
        // Crush — purple
        "crush" => Color::Rgb(150, 70, 200),
        // Gemini — Google blue
        "gemini" => Color::Rgb(66, 133, 244),
        // Kiro — teal
        "kiro" => Color::Rgb(0, 188, 188),
        // OpenCode — yellow-amber
        "opencode" => Color::Rgb(240, 185, 11),
        // Vibe — pink/magenta
        "vibe" => Color::Rgb(220, 60, 130),
        // Fallback
        _ => Color::White,
    }
}

/// Return a Style with the agent's brand foreground colour.
pub fn agent_style(agent: &str) -> Style {
    Style::new().fg(agent_color(agent))
}
