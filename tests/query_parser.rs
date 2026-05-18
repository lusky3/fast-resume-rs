/// Query-parser tests — port of tests/test_query.py.
///
/// Mirrors the Python test suite closely so that the two implementations can
/// be verified against the same expectations.

use fr::query::{DateOp, Filter, parse_query};

// ─────────────────────────────────────────────────────────────────────────────
// Basic keyword extraction
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_no_keywords() {
    let q = parse_query("api auth bug");
    assert_eq!(q.text, "api auth bug");
    assert!(q.agent.is_none());
    assert!(q.directory.is_none());
    assert!(q.date.is_none());
}

#[test]
fn test_agent_keyword() {
    let q = parse_query("agent:claude api auth");
    assert_eq!(q.text, "api auth");
    let agent = q.agent.expect("agent filter should be present");
    assert_eq!(agent.include, vec!["claude"]);
    assert!(agent.exclude.is_empty());
    assert!(!agent.negated());
    assert!(q.directory.is_none());
    assert!(q.date.is_none());
}

#[test]
fn test_dir_keyword() {
    let q = parse_query("dir:my-project bug fix");
    assert_eq!(q.text, "bug fix");
    assert!(q.agent.is_none());
    let dir = q.directory.expect("directory filter should be present");
    assert_eq!(dir.include, vec!["my-project"]);
    assert!(q.date.is_none());
}

#[test]
fn test_both_keywords() {
    let q = parse_query("agent:claude dir:my-project auth");
    assert_eq!(q.text, "auth");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["claude"]);
    let dir = q.directory.expect("directory");
    assert_eq!(dir.include, vec!["my-project"]);
    assert!(q.date.is_none());
}

#[test]
fn test_keywords_at_end() {
    let q = parse_query("auth bug agent:codex");
    assert_eq!(q.text, "auth bug");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["codex"]);
}

#[test]
fn test_keywords_in_middle() {
    let q = parse_query("api agent:claude auth");
    assert_eq!(q.text, "api auth");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["claude"]);
}

#[test]
fn test_empty_query() {
    let q = parse_query("");
    assert_eq!(q.text, "");
    assert!(q.agent.is_none());
    assert!(q.directory.is_none());
    assert!(q.date.is_none());
}

#[test]
fn test_only_keywords() {
    let q = parse_query("agent:claude dir:project");
    assert_eq!(q.text, "");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["claude"]);
    let dir = q.directory.expect("dir");
    assert_eq!(dir.include, vec!["project"]);
}

#[test]
fn test_duplicate_keyword_last_wins() {
    let q = parse_query("agent:claude agent:codex api");
    assert_eq!(q.text, "api");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["codex"]);
}

#[test]
fn test_whitespace_handling() {
    let q = parse_query("  agent:claude   api   auth  ");
    assert_eq!(q.text, "api auth");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["claude"]);
}

#[test]
fn test_hyphenated_values() {
    let q = parse_query("agent:copilot-cli api");
    assert_eq!(q.text, "api");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["copilot-cli"]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Multiple comma-separated values
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_agent_multiple_values() {
    let q = parse_query("agent:claude,codex api");
    assert_eq!(q.text, "api");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["claude", "codex"]);
    assert!(!agent.negated());
}

#[test]
fn test_dir_multiple_values() {
    let q = parse_query("dir:proj1,proj2");
    let dir = q.directory.expect("dir");
    assert_eq!(dir.include, vec!["proj1", "proj2"]);
}

#[test]
fn test_three_values() {
    let q = parse_query("agent:claude,codex,vibe");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["claude", "codex", "vibe"]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Negation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_negation_with_dash_prefix() {
    let q = parse_query("-agent:claude api");
    assert_eq!(q.text, "api");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.exclude, vec!["claude"]);
    assert!(agent.include.is_empty());
    assert!(agent.negated());
}

#[test]
fn test_negation_with_exclamation() {
    let q = parse_query("agent:!claude api");
    assert_eq!(q.text, "api");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.exclude, vec!["claude"]);
    assert!(agent.include.is_empty());
    assert!(agent.negated());
}

#[test]
fn test_negation_dir() {
    let q = parse_query("-dir:project api");
    let dir = q.directory.expect("dir");
    assert_eq!(dir.exclude, vec!["project"]);
    assert!(dir.include.is_empty());
    assert!(dir.negated());
}

#[test]
fn test_negation_with_multiple_values() {
    let q = parse_query("-agent:claude,codex");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.exclude, vec!["claude", "codex"]);
    assert!(agent.include.is_empty());
    assert!(agent.negated());
}

#[test]
fn test_negation_date() {
    let q = parse_query("-date:today");
    let date = q.date.expect("date filter");
    assert!(date.negated);
}

#[test]
fn test_negation_date_with_exclamation() {
    let q = parse_query("date:!today");
    let date = q.date.expect("date filter");
    assert!(date.negated);
}

// ─────────────────────────────────────────────────────────────────────────────
// Mixed include / exclude
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_mixed_agent_filter() {
    let q = parse_query("agent:claude,!codex api");
    assert_eq!(q.text, "api");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["claude"]);
    assert_eq!(agent.exclude, vec!["codex"]);
    assert!(!agent.negated()); // has includes, so not exclude-only
}

#[test]
fn test_mixed_dir_filter() {
    let q = parse_query("dir:proj1,!proj2");
    let dir = q.directory.expect("dir");
    assert_eq!(dir.include, vec!["proj1"]);
    assert_eq!(dir.exclude, vec!["proj2"]);
}

#[test]
fn test_mixed_multiple_includes_one_exclude() {
    let q = parse_query("agent:claude,codex,!vibe");
    let agent = q.agent.expect("agent");
    assert_eq!(agent.include, vec!["claude", "codex"]);
    assert_eq!(agent.exclude, vec!["vibe"]);
}

#[test]
fn test_dash_prefix_makes_all_excludes() {
    let q = parse_query("-agent:claude,codex");
    let agent = q.agent.expect("agent");
    assert!(agent.include.is_empty());
    assert_eq!(agent.exclude, vec!["claude", "codex"]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Filter::matches
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_filter_single_value_matches() {
    let f = Filter { include: vec!["claude".to_owned()], exclude: vec![] };
    assert!(f.matches("claude", false));
    assert!(!f.matches("codex", false));
}

#[test]
fn test_filter_multiple_values_or() {
    let f = Filter {
        include: vec!["claude".to_owned(), "codex".to_owned()],
        exclude: vec![],
    };
    assert!(f.matches("claude", false));
    assert!(f.matches("codex", false));
    assert!(!f.matches("vibe", false));
}

#[test]
fn test_filter_negated() {
    let f = Filter { include: vec![], exclude: vec!["claude".to_owned()] };
    assert!(!f.matches("claude", false));
    assert!(f.matches("codex", false));
}

#[test]
fn test_filter_substring_match() {
    let f = Filter { include: vec!["project".to_owned()], exclude: vec![] };
    assert!(f.matches("/home/user/project/src", true));
    assert!(!f.matches("/home/user/other", true));
}

#[test]
fn test_filter_substring_negated() {
    let f = Filter { include: vec![], exclude: vec!["project".to_owned()] };
    assert!(!f.matches("/home/user/project/src", true));
    assert!(f.matches("/home/user/other", true));
}

#[test]
fn test_filter_empty_matches_all() {
    let f = Filter::default();
    assert!(f.matches("anything", false));
}

#[test]
fn test_filter_mixed_include_exclude() {
    let f = Filter {
        include: vec!["claude".to_owned()],
        exclude: vec!["codex".to_owned()],
    };
    assert!(f.matches("claude", false));
    assert!(!f.matches("codex", false));
    assert!(!f.matches("vibe", false)); // not in include
}

#[test]
fn test_filter_exclude_takes_precedence() {
    // Same value in both include and exclude — exclude wins.
    let f = Filter {
        include: vec!["claude".to_owned()],
        exclude: vec!["claude".to_owned()],
    };
    assert!(!f.matches("claude", false));
}

// ─────────────────────────────────────────────────────────────────────────────
// Date filters
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_date_today() {
    let q = parse_query("date:today api");
    assert_eq!(q.text, "api");
    let date = q.date.expect("date filter");
    assert_eq!(date.op, DateOp::Exact);
    assert_eq!(date.value, "today");
    assert!(!date.negated);
    // Cutoff should be within the past 24 hours.
    let now = jiff::Timestamp::now();
    let diff = now.as_second() - date.cutoff.as_second();
    assert!(diff >= 0 && diff < 86_400, "today cutoff should be within today");
}

#[test]
fn test_date_yesterday() {
    let q = parse_query("date:yesterday");
    let date = q.date.expect("date filter");
    assert_eq!(date.op, DateOp::Exact);
    assert_eq!(date.value, "yesterday");
}

#[test]
fn test_date_less_than_hours() {
    let q = parse_query("date:<1h api");
    assert_eq!(q.text, "api");
    let date = q.date.expect("date filter");
    assert_eq!(date.op, DateOp::LessThan);
    // Cutoff should be approximately 1 hour ago.
    let now = jiff::Timestamp::now();
    let diff = (now.as_second() - date.cutoff.as_second()).unsigned_abs();
    let expected = 3_600u64;
    assert!(diff.abs_diff(expected) < 5, "cutoff should be ~1h ago");
}

#[test]
fn test_date_less_than_days() {
    let q = parse_query("date:<2d");
    let date = q.date.expect("date filter");
    assert_eq!(date.op, DateOp::LessThan);
    let now = jiff::Timestamp::now();
    let diff = (now.as_second() - date.cutoff.as_second()).unsigned_abs();
    let expected = 2 * 86_400u64;
    assert!(diff.abs_diff(expected) < 5);
}

#[test]
fn test_date_greater_than() {
    let q = parse_query("date:>1d");
    let date = q.date.expect("date filter");
    assert_eq!(date.op, DateOp::GreaterThan);
}

#[test]
fn test_date_without_operator() {
    let q = parse_query("date:1h");
    let date = q.date.expect("date filter");
    assert_eq!(date.op, DateOp::LessThan);
}

#[test]
fn test_date_week_shortcut() {
    let q = parse_query("date:week");
    let date = q.date.expect("date filter");
    assert_eq!(date.op, DateOp::LessThan);
}

#[test]
fn test_date_month_shortcut() {
    let q = parse_query("date:month");
    let date = q.date.expect("date filter");
    assert_eq!(date.op, DateOp::LessThan);
}

#[test]
fn test_date_invalid() {
    let q = parse_query("date:invalid");
    assert!(q.date.is_none(), "invalid date value should produce no filter");
}

#[test]
fn test_date_combined_with_other_filters() {
    let q = parse_query("agent:claude date:<1d dir:project api");
    assert_eq!(q.text, "api");
    assert!(q.agent.is_some());
    assert!(q.directory.is_some());
    assert!(q.date.is_some());
    let date = q.date.unwrap();
    assert_eq!(date.op, DateOp::LessThan);
}

// ─────────────────────────────────────────────────────────────────────────────
// Autocomplete suggestion helper
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_suggestion_agent_partial() {
    let sug = fr::tui::compute_suggestion("agent:cl");
    assert_eq!(sug, Some("agent:claude".to_owned()));
}

#[test]
fn test_suggestion_agent_no_match() {
    let sug = fr::tui::compute_suggestion("agent:zzz");
    assert!(sug.is_none());
}

#[test]
fn test_suggestion_empty() {
    let sug = fr::tui::compute_suggestion("");
    assert!(sug.is_none());
}

#[test]
fn test_suggestion_plain_text() {
    let sug = fr::tui::compute_suggestion("hello world");
    assert!(sug.is_none());
}

#[test]
fn test_suggestion_agent_full_name_no_complete() {
    // Complete name should not generate a suggestion (nothing to complete).
    let sug = fr::tui::compute_suggestion("agent:claude");
    assert!(sug.is_none(), "already complete — no suggestion expected");
}

#[test]
fn test_suggestion_agent_with_following_space() {
    // Once a space appears after the value, don't suggest.
    let sug = fr::tui::compute_suggestion("agent:claude more text");
    assert!(sug.is_none());
}
