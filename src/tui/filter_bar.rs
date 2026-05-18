//! Filter bar — a horizontal row of agent-selector buttons.
//!
//! Each button displays a coloured text badge (and optionally an icon from
//! `IconCache`).  The active agent filter highlights with `Modifier::REVERSED`.
//! Pressing Tab cycles through agents; pressing Enter on a highlighted button
//! (or pressing a button's shortcut) toggles the filter.
//!
//! State lives in `app::State`:
//! ```text
//! pub active_agent_filter: Option<String>,  // None = show all
//! ```

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::{icons::IconCache, style::agent_color};

/// The canonical ordered list of agent slugs shown in the filter bar.
pub const FILTER_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "copilot-cli",
    "copilot-vscode",
    "crush",
    "gemini",
    "kiro",
    "opencode",
    "vibe",
];

/// Draw the filter bar into `area`.
///
/// * `active_filter` — the currently active agent slug (`None` = "all").
/// * `icons` — shared icon cache; icons are rendered in each button cell when
///   an icon PNG is available.  Rendering only happens in fixed-position cells
///   per the migration-plan restriction.
pub fn draw_filter_bar(
    f: &mut Frame,
    area: Rect,
    active_filter: Option<&str>,
    icons: &mut IconCache,
) {
    if area.height == 0 {
        return;
    }

    // One slot per agent plus one "All" slot at the left.
    let slot_count = FILTER_AGENTS.len() + 1;
    let constraints: Vec<Constraint> = (0..slot_count)
        .map(|_| Constraint::Ratio(1, slot_count as u32))
        .collect();

    let slots = Layout::horizontal(constraints).split(area);

    // "All" button
    draw_button(f, slots[0], "all", "All", active_filter.is_none(), icons);

    // Per-agent buttons
    for (i, agent) in FILTER_AGENTS.iter().enumerate() {
        let is_active = active_filter == Some(agent);
        draw_button(f, slots[i + 1], agent, agent, is_active, icons);
    }
}

/// Draw a single filter button.
///
/// `label` is the display text; `agent` is the slug used to look up the colour
/// and icon.  When `is_active` the button is rendered with `Modifier::REVERSED`.
fn draw_button(
    f: &mut Frame,
    area: Rect,
    agent: &str,
    label: &str,
    is_active: bool,
    icons: &mut IconCache,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let fg = if agent == "all" {
        ratatui::style::Color::White
    } else {
        agent_color(agent)
    };

    let base_style = Style::default().fg(fg);
    let style = if is_active {
        base_style.add_modifier(Modifier::REVERSED)
    } else {
        base_style
    };

    // Decide whether there is room to render an icon (minimum 3 cols wide).
    // Icons go in a 2-column-wide sub-rect at the left of the button interior.
    let has_icon_room = area.width >= 4 && area.height >= 1;
    let show_icon = has_icon_room && agent != "all";

    if show_icon {
        // Split the button into [icon_cell | text_cell].
        let inner_chunks = Layout::horizontal([
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(area);

        let icon_rect = inner_chunks[0];
        let text_rect = inner_chunks[1];

        // Render the icon into the fixed-position icon cell.
        // Falls through to a blank cell when no PNG is available — no crash.
        icons.render_icon(f, agent, icon_rect);

        // Render the text badge in the remaining space.
        let short = shorten_agent(label);
        let para = Paragraph::new(Line::from(Span::styled(short, style)));
        f.render_widget(para, text_rect);
    } else {
        // No icon: render a centred text label with a thin border.
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(ratatui::style::Color::DarkGray));
        let inner = block.inner(area);

        let short = shorten_agent(label);
        let para = Paragraph::new(Line::from(Span::styled(short, style)));
        f.render_widget(block, area);
        f.render_widget(para, inner);
    }
}

/// Shorten an agent slug to fit in a narrow button cell.
fn shorten_agent(agent: &str) -> String {
    match agent {
        "copilot-cli" => "cpt-cli".to_string(),
        "copilot-vscode" => "cpt-vsc".to_string(),
        "opencode" => "opencode".to_string(),
        other => other.to_string(),
    }
}
