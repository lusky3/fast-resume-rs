/// Launch modal for yolo-capable sessions.
///
/// Rendered as a centered overlay using `ratatui::widgets::Clear` to wipe the
/// background, then a bordered block with:
///   - A yolo checkbox row
///   - Two action buttons: [Cancel] and [Launch]
///   - A hint line at the bottom
///
/// Ported from python/fast_resume/tui/modal.py (section 6 of the migration plan).
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

/// Which button in the modal has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalFocus {
    Cancel,
    #[default]
    Launch,
}

impl ModalFocus {
    pub fn toggle(self) -> Self {
        match self {
            Self::Cancel => Self::Launch,
            Self::Launch => Self::Cancel,
        }
    }
}

/// Compute a centred rect that is `pct_w`% wide and `pct_h`% tall of `area`.
pub fn centered_rect(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - pct_h) / 2),
        Constraint::Percentage(pct_h),
        Constraint::Percentage((100 - pct_h) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - pct_w) / 2),
        Constraint::Percentage(pct_w),
        Constraint::Percentage((100 - pct_w) / 2),
    ])
    .split(vertical[1])[1]
}

/// Draw the yolo launch modal.
///
/// - `yolo` — current state of the yolo checkbox
/// - `focus` — which button has keyboard focus
pub fn draw_modal(f: &mut Frame, area: Rect, yolo: bool, focus: ModalFocus) {
    let modal_area = centered_rect(52, 32, area);

    // Wipe the background behind the modal.
    f.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Launch session ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    f.render_widget(block, modal_area);

    // Inner area for content.
    let inner = modal_area.inner(Margin { horizontal: 2, vertical: 1 });
    if inner.height < 3 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // yolo checkbox
        Constraint::Length(1), // spacing
        Constraint::Length(1), // buttons
        Constraint::Length(1), // spacing
        Constraint::Length(1), // hint
    ])
    .split(inner);

    // ── Yolo checkbox ────────────────────────────────────────────────────────
    let checkbox = if yolo { "[x]" } else { "[ ]" };
    let yolo_line = Line::from(vec![
        Span::styled(
            checkbox,
            Style::default()
                .fg(if yolo { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("Yolo mode", Style::default().fg(Color::Yellow)),
        Span::styled(
            " (auto-approve all prompts)",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(yolo_line), rows[0]);

    // ── Buttons ───────────────────────────────────────────────────────────────
    let button_area = rows[2];
    let btn_cols = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .split(button_area);

    draw_button(f, btn_cols[0], "Cancel", focus == ModalFocus::Cancel);
    draw_button(f, btn_cols[1], "Launch", focus == ModalFocus::Launch);

    // ── Hint line ─────────────────────────────────────────────────────────────
    let hint = Span::styled(
        "Space/y/n: toggle yolo · Tab/←/→: switch · Enter: confirm · Esc: cancel",
        Style::default().fg(Color::DarkGray),
    );
    f.render_widget(Paragraph::new(Line::from(hint)), rows[4]);
}

/// Draw a single button, highlighted when it has focus.
fn draw_button(f: &mut Frame, area: Rect, label: &str, focused: bool) {
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let text = format!("[ {label} ]");
    f.render_widget(Paragraph::new(Span::styled(text, style)), area);
}
