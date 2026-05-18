/// Keyword DSL parser for fast-resume search queries.
///
/// Supports syntax like: `agent:claude,codex dir:my-project date:<1d api auth`
///
/// Ported from python/fast_resume/query.py.
///
/// Keywords:
/// - `agent:claude` / `agent:claude,codex` (OR) / `-agent:vibe` or `agent:!vibe` (exclude)
/// - `dir:my-project` (substring) / `-dir:test`
/// - `date:today` / `date:yesterday` / `date:<1h` / `date:<2d` / `date:>1w` /
///   `date:week` / `date:month`
use regex::Regex;
use std::sync::OnceLock;

use jiff::Timestamp;

/// A filter with multiple possible values and negation support.
///
/// Supports mixed include/exclude: `agent:claude,!codex` means
/// "match claude but not codex".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    /// Values to match (OR logic).
    pub include: Vec<String>,
    /// Values to exclude (AND NOT logic).
    pub exclude: Vec<String>,
}

impl Filter {
    /// All values (include + exclude).
    pub fn values(&self) -> Vec<&str> {
        self.include
            .iter()
            .chain(self.exclude.iter())
            .map(|s| s.as_str())
            .collect()
    }

    /// True if the filter is empty (no include or exclude).
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// True if the filter is exclude-only (no include values).
    pub fn negated(&self) -> bool {
        self.include.is_empty() && !self.exclude.is_empty()
    }

    /// Check if `value` matches this filter.
    ///
    /// When `substring` is true, the check tests whether any filter value is a
    /// case-insensitive substring of `value`.  Otherwise it's an exact match.
    pub fn matches(&self, value: &str, substring: bool) -> bool {
        if self.include.is_empty() && self.exclude.is_empty() {
            return true;
        }

        let check = |filter_val: &str| -> bool {
            if substring {
                value.to_lowercase().contains(&filter_val.to_lowercase())
            } else {
                value == filter_val
            }
        };

        // Excludes take precedence.
        if self.exclude.iter().any(|v| check(v)) {
            return false;
        }

        // If there are no includes, accept (exclude-only filter).
        if self.include.is_empty() {
            return true;
        }

        // At least one include must match.
        self.include.iter().any(|v| check(v))
    }
}

/// Date filter comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateOp {
    /// Named date (today, yesterday) — match sessions >= cutoff within that day.
    Exact,
    /// `<1h` — sessions newer than N units (timestamp >= cutoff).
    LessThan,
    /// `>1d` — sessions older than N units (timestamp <= cutoff).
    GreaterThan,
}

/// Parsed date filter.
#[derive(Debug, Clone)]
pub struct DateFilter {
    pub op: DateOp,
    /// Original value string for display.
    pub value: String,
    /// The resolved cutoff timestamp computed at parse time.
    pub cutoff: Timestamp,
    /// True if the filter should exclude matching sessions.
    pub negated: bool,
}

/// Result of parsing a search query.
#[derive(Debug, Clone, Default)]
pub struct ParsedQuery {
    /// Remaining free-text search terms (keywords stripped out).
    pub text: String,
    /// Extracted agent filter.
    pub agent: Option<Filter>,
    /// Extracted directory filter.
    pub directory: Option<Filter>,
    /// Extracted date filter.
    pub date: Option<DateFilter>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Compiled keyword pattern (lazy, reused across calls).
fn keyword_pattern() -> &'static Regex {
    static PAT: OnceLock<Regex> = OnceLock::new();
    PAT.get_or_init(|| {
        // Matches:  (-?)(agent|dir|date):("value with spaces"|unquoted_value)
        Regex::new(r#"(-?)(agent|dir|date):(?:"([^"]+)"|(\S+))"#)
            .expect("keyword regex is valid")
    })
}

/// Relative-time pattern: `[<>]?(\d+)(m|h|d|w|mo|y)`.
fn relative_time_pattern() -> &'static Regex {
    static PAT: OnceLock<Regex> = OnceLock::new();
    PAT.get_or_init(|| {
        Regex::new(r"^([<>])?(\d+)(m|h|d|w|mo|y)$").expect("relative time regex is valid")
    })
}

fn parse_date_value(value: &str, negated: bool) -> Option<DateFilter> {
    let now = Timestamp::now();

    // Handle `!` prefix for negation inside the value.
    let (value, negated) = if let Some(rest) = value.strip_prefix('!') {
        (rest, true)
    } else {
        (value, negated)
    };

    let lower = value.to_lowercase();

    // Named shortcuts.
    match lower.as_str() {
        "today" => {
            // Midnight local today.
            let cutoff = day_start(now);
            return Some(DateFilter {
                op: DateOp::Exact,
                value: value.to_owned(),
                cutoff,
                negated,
            });
        }
        "yesterday" => {
            let cutoff = day_start(now) - jiff::SignedDuration::from_secs(86400);
            return Some(DateFilter {
                op: DateOp::Exact,
                value: value.to_owned(),
                cutoff,
                negated,
            });
        }
        "week" => {
            let cutoff = now - jiff::SignedDuration::from_secs(7 * 86400);
            return Some(DateFilter {
                op: DateOp::LessThan,
                value: value.to_owned(),
                cutoff,
                negated,
            });
        }
        "month" => {
            let cutoff = now - jiff::SignedDuration::from_secs(30 * 86400);
            return Some(DateFilter {
                op: DateOp::LessThan,
                value: value.to_owned(),
                cutoff,
                negated,
            });
        }
        _ => {}
    }

    // Relative-time patterns.
    if let Some(caps) = relative_time_pattern().captures(&lower) {
        let op_str = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let num: i64 = caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let unit = caps.get(3).map(|m| m.as_str()).unwrap_or("s");

        let seconds = num * time_unit_seconds(unit);
        let cutoff = now - jiff::SignedDuration::from_secs(seconds);

        let op = if op_str == ">" {
            DateOp::GreaterThan
        } else {
            DateOp::LessThan
        };

        return Some(DateFilter {
            op,
            value: value.to_owned(),
            cutoff,
            negated,
        });
    }

    None
}

/// Return the number of seconds for a time-unit abbreviation.
fn time_unit_seconds(unit: &str) -> i64 {
    match unit {
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "mo" => 30 * 86_400,
        "y" => 365 * 86_400,
        _ => 1,
    }
}

/// Return `ts` rounded back to midnight UTC (approximates "start of day").
fn day_start(ts: Timestamp) -> Timestamp {
    let secs = ts.as_second();
    let day_secs = secs - (secs % 86_400);
    Timestamp::new(day_secs, 0).unwrap_or(ts)
}

fn parse_filter_value(value: &str, negated: bool) -> Filter {
    let mut include = Vec::new();
    let mut exclude = Vec::new();

    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(rest) = part.strip_prefix('!') {
            exclude.push(rest.to_owned());
        } else if negated {
            exclude.push(part.to_owned());
        } else {
            include.push(part.to_owned());
        }
    }

    Filter { include, exclude }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Parse keyword syntax from a query string.
///
/// Returns a [`ParsedQuery`] with extracted filters and the remaining free-text.
///
/// # Examples
///
/// ```
/// use fr::query::parse_query;
///
/// let q = parse_query("agent:claude api auth");
/// assert_eq!(q.text, "api auth");
/// assert!(q.agent.is_some());
///
/// let q2 = parse_query("-agent:vibe");
/// assert!(q2.agent.unwrap().negated());
/// ```
pub fn parse_query(s: &str) -> ParsedQuery {
    let pat = keyword_pattern();

    let mut agent: Option<Filter> = None;
    let mut directory: Option<Filter> = None;
    let mut date: Option<DateFilter> = None;

    for caps in pat.captures_iter(s) {
        let neg_prefix = &caps[1] == "-";
        let keyword = &caps[2];
        // Value is either quoted (group 3) or unquoted (group 4).
        let value = caps
            .get(3)
            .or_else(|| caps.get(4))
            .map(|m| m.as_str())
            .unwrap_or("");

        match keyword {
            "agent" => agent = Some(parse_filter_value(value, neg_prefix)),
            "dir" => directory = Some(parse_filter_value(value, neg_prefix)),
            "date" => date = parse_date_value(value, neg_prefix),
            _ => {}
        }
    }

    // Strip keyword:value tokens and normalise whitespace to get free text.
    let text = pat.replace_all(s, "");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");

    ParsedQuery {
        text,
        agent,
        directory,
        date,
    }
}
