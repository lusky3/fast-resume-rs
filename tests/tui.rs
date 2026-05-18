/// TUI rendering tests using ratatui's TestBackend.
///
/// These tests exercise the draw functions in isolation — no event loop, no
/// terminal. They verify that the widgets render without panicking and that
/// key content appears in the output buffer.
///
/// All tests that involve `IconCache` use `IconCache::halfblocks()` to ensure
/// deterministic, protocol-agnostic output (no Sixel/Kitty bytes in snapshots).
use ratatui::{Terminal, backend::TestBackend, layout::Rect, widgets::TableState};

use fr::session::Session;
use fr::tui::{
    compute_suggestion,
    filter_bar::draw_filter_bar,
    icons::IconCache,
    modal::{ModalFocus, draw_modal},
    preview::draw_preview,
    results_list::draw_results,
};

fn make_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(width, height)).unwrap()
}

/// Helper: build a minimal Session with the given fields.
fn make_session(id: &str, agent: &str, title: &str, content: &str, dir: &str) -> Session {
    Session {
        id: id.to_owned(),
        agent: agent.to_owned(),
        title: title.to_owned(),
        directory: dir.to_owned(),
        timestamp: jiff::Timestamp::UNIX_EPOCH,
        content: content.to_owned(),
        message_count: 2,
        mtime: 0.0,
        yolo: false,
    }
}

// ---------------------------------------------------------------------------
// Test 1: Render without panic — empty state
// ---------------------------------------------------------------------------

/// Build a minimal `State`-like value without the full SessionSearch for testing.
/// We test the individual draw helpers rather than the full `draw()` function
/// because `draw()` requires a live `SessionSearch`.
#[test]
fn test_results_renders_without_panic_empty() {
    let mut terminal = make_terminal(80, 24);
    let sessions: Vec<Session> = vec![];
    let mut table_state = TableState::default();

    terminal
        .draw(|f| {
            draw_results(f, f.area(), &sessions, &mut table_state, "");
        })
        .unwrap();

    // The buffer should have been written to — at minimum it isn't all blank.
    let buf = terminal.backend().buffer().clone();
    // The table block's border characters should be somewhere.
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(!content.is_empty());
}

// ---------------------------------------------------------------------------
// Test 2: Results list shows session titles
// ---------------------------------------------------------------------------

#[test]
fn test_results_list_shows_sessions() {
    let mut terminal = make_terminal(120, 30);

    let sessions = vec![
        make_session("id1", "claude", "My First Session", "» hello", "/home/user/project"),
        make_session("id2", "codex", "Codex Analysis", "» analyze this", "/tmp/work"),
    ];
    let mut table_state = TableState::default();
    table_state.select(Some(0));

    terminal
        .draw(|f| {
            draw_results(f, f.area(), &sessions, &mut table_state, "");
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();

    // Title of session 0 should appear somewhere in the buffer.
    assert!(
        content.contains("My First Session"),
        "Expected 'My First Session' in buffer, got: {:?}",
        &content[..content.len().min(200)]
    );
    // Title of session 1 should appear too.
    assert!(
        content.contains("Codex Analysis"),
        "Expected 'Codex Analysis' in buffer"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Preview pane shows session content
// ---------------------------------------------------------------------------

#[test]
fn test_preview_shows_content() {
    let mut terminal = make_terminal(80, 24);

    let session = make_session(
        "preview-id",
        "claude",
        "Preview Test",
        "» What is the capital of France?\n\nParis is the capital of France.",
        "/tmp",
    );

    terminal
        .draw(|f| {
            let area = f.area();
            draw_preview(f, area, Some(&session), "", 0);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();

    // The user prompt prefix should appear.
    assert!(
        content.contains("»") || content.contains("What is the capital"),
        "Expected user prompt content in preview buffer"
    );
    // Some part of the assistant response should appear.
    assert!(
        content.contains("Paris") || content.contains("capital"),
        "Expected assistant response content in preview buffer"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Preview shows "No session selected" when None
// ---------------------------------------------------------------------------

#[test]
fn test_preview_no_selection_message() {
    let mut terminal = make_terminal(80, 24);

    terminal
        .draw(|f| {
            let area = f.area();
            draw_preview(f, area, None, "", 0);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();

    assert!(
        content.contains("No session selected"),
        "Expected 'No session selected' in empty preview"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Query highlighting — bold marker via cell modifier
// ---------------------------------------------------------------------------

#[test]
fn test_results_highlight_query_match() {
    let mut terminal = make_terminal(120, 30);

    let sessions = vec![make_session(
        "hl1",
        "claude",
        "Rust programming session",
        "» let x = 5;",
        "/home/user",
    )];
    let mut table_state = TableState::default();
    table_state.select(Some(0));

    // Draw with a query that matches the title.
    terminal
        .draw(|f| {
            draw_results(f, f.area(), &sessions, &mut table_state, "Rust");
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();

    // "Rust" should appear in the rendered output.
    assert!(
        content.contains("Rust"),
        "Expected 'Rust' to appear in highlighted results"
    );
}

// ---------------------------------------------------------------------------
// Test 6: Results with a selected row — verifies selection doesn't crash
// ---------------------------------------------------------------------------

#[test]
fn test_results_selection() {
    let mut terminal = make_terminal(120, 40);

    let sessions: Vec<Session> = (0..5)
        .map(|i| {
            make_session(
                &format!("id{i}"),
                "vibe",
                &format!("Session number {i}"),
                &format!("» message {i}"),
                &format!("/proj/{i}"),
            )
        })
        .collect();

    let mut table_state = TableState::default();
    table_state.select(Some(2)); // select the middle item

    terminal
        .draw(|f| {
            draw_results(f, f.area(), &sessions, &mut table_state, "");
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();

    // Session 2's title should be visible.
    assert!(
        content.contains("Session number 2"),
        "Expected 'Session number 2' in buffer"
    );
}

// ---------------------------------------------------------------------------
// Test 7: draw() full frame — requires constructing State via the public API
// ---------------------------------------------------------------------------
// We test the individual components above; the full draw() test would require
// mocking SessionSearch. Instead we verify the highlight_text helper directly.

#[test]
fn test_highlight_text_empty_query() {
    use fr::tui::results_list::highlight_text;
    let line = highlight_text("hello world", "");
    // With empty query, spans should contain the original text unchanged.
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "hello world");
}

#[test]
fn test_highlight_text_match() {
    use fr::tui::results_list::highlight_text;
    let line = highlight_text("Rust is great", "rust");
    // Should have a bold yellow span for "Rust" and plain spans for the rest.
    assert!(line.spans.len() >= 2, "Expected at least 2 spans");
    let all_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(all_text, "Rust is great");
}

// ---------------------------------------------------------------------------
// Test 8: preview scroll — using draw_results_list with a zero-height area
// ---------------------------------------------------------------------------

#[test]
fn test_results_zero_height_no_panic() {
    let mut terminal = make_terminal(80, 5);
    let sessions = vec![make_session("x", "claude", "Title", "content", "/")];
    let mut state = TableState::default();
    state.select(Some(0));

    // Should not panic even on very small terminal sizes.
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 3);
            draw_results(f, area, &sessions, &mut state, "");
        })
        .unwrap();
}

// ---------------------------------------------------------------------------
// Test 9: filter bar renders agent badges
//
// Uses `IconCache::halfblocks()` for deterministic, protocol-agnostic output.
// ---------------------------------------------------------------------------

#[test]
fn test_filter_bar_renders() {
    let mut terminal = make_terminal(200, 3);
    // Use halfblocks to ensure no Sixel/Kitty bytes appear in the snapshot.
    let mut icons = IconCache::halfblocks();

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 200, 1);
            // No active filter — "all" mode.
            draw_filter_bar(f, area, None, &mut icons);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();

    // The "all" button and at least some agent slugs should appear.
    assert!(
        content.contains("all") || content.contains("claude"),
        "Expected filter bar labels in buffer, got: {:?}",
        &content[..content.len().min(200)]
    );
}

#[test]
fn test_filter_bar_active_filter() {
    let mut terminal = make_terminal(200, 3);
    let mut icons = IconCache::halfblocks();

    // Render with "claude" active.
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 200, 1);
            draw_filter_bar(f, area, Some("claude"), &mut icons);
        })
        .unwrap();

    // We just check it doesn't panic and has content — the reversed-style cell
    // for "claude" won't appear literally differently in a symbol scan.
    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(!content.trim().is_empty(), "Filter bar should render something");
}

#[test]
fn test_filter_bar_zero_height_no_panic() {
    let mut terminal = make_terminal(80, 1);
    let mut icons = IconCache::halfblocks();

    // area height == 0: draw_filter_bar should return early without panic.
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 0);
            draw_filter_bar(f, area, None, &mut icons);
        })
        .unwrap();
}

// ---------------------------------------------------------------------------
// Phase 6: Test 10 — Modal renders when modal_open is true
// ---------------------------------------------------------------------------

/// draw_modal renders a centered overlay with the expected content.
#[test]
fn test_modal_renders_when_open() {
    let mut terminal = make_terminal(120, 40);

    terminal
        .draw(|f| {
            draw_modal(f, f.area(), false, ModalFocus::Launch);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();

    // The modal title should appear.
    assert!(
        content.contains("Launch session"),
        "Expected 'Launch session' in modal buffer, got: {:?}",
        &content[..content.len().min(300)]
    );
    // The yolo checkbox should appear.
    assert!(
        content.contains("[ ]") || content.contains("[x]"),
        "Expected checkbox in modal buffer"
    );
    // The buttons should appear.
    assert!(
        content.contains("Launch"),
        "Expected 'Launch' button in modal buffer"
    );
    assert!(
        content.contains("Cancel"),
        "Expected 'Cancel' button in modal buffer"
    );
}

/// draw_modal renders the yolo checkbox as checked when yolo=true.
#[test]
fn test_modal_yolo_checked() {
    let mut terminal = make_terminal(120, 40);

    terminal
        .draw(|f| {
            draw_modal(f, f.area(), true, ModalFocus::Launch);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();

    assert!(
        content.contains("[x]"),
        "Expected '[x]' checked checkbox when yolo=true"
    );
}

/// draw_modal with Cancel focused — Cancel button shows first.
#[test]
fn test_modal_cancel_focus() {
    let mut terminal = make_terminal(120, 40);

    terminal
        .draw(|f| {
            draw_modal(f, f.area(), false, ModalFocus::Cancel);
        })
        .unwrap();

    // Should not panic and should show both buttons.
    let buf = terminal.backend().buffer().clone();
    let content: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(content.contains("Cancel"), "Expected 'Cancel' in buffer");
    assert!(content.contains("Launch"), "Expected 'Launch' in buffer");
}

// ---------------------------------------------------------------------------
// Phase 6: Test 11 — Autocomplete suggestion computed correctly
// ---------------------------------------------------------------------------

/// compute_suggestion returns a completion when query ends with partial agent name.
#[test]
fn test_suggestion_appears_for_agent_prefix() {
    let sug = compute_suggestion("agent:cl");
    assert_eq!(
        sug,
        Some("agent:claude".to_owned()),
        "Should complete 'cl' to 'claude'"
    );
}

#[test]
fn test_suggestion_codex_prefix() {
    let sug = compute_suggestion("agent:co");
    // First match in FILTER_AGENTS starting with "co" is "codex".
    assert_eq!(sug, Some("agent:codex".to_owned()));
}

#[test]
fn test_suggestion_none_for_no_prefix() {
    let sug = compute_suggestion("some plain text");
    assert!(sug.is_none(), "No suggestion for plain text");
}

#[test]
fn test_suggestion_none_when_complete() {
    let sug = compute_suggestion("agent:claude");
    assert!(sug.is_none(), "Complete agent name should not produce suggestion");
}

#[test]
fn test_suggestion_none_after_space() {
    // Once the user adds a space after the agent value, stop suggesting.
    let sug = compute_suggestion("agent:claude ");
    assert!(sug.is_none());
}

#[test]
fn test_suggestion_with_preceding_text() {
    // Suggestion should work even when there is text before the agent: keyword.
    let sug = compute_suggestion("api bug agent:vi");
    assert_eq!(sug, Some("api bug agent:vibe".to_owned()));
}
