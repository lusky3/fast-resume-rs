// Items defined in sub-modules but only used in integration tests appear as dead code
// to the binary. Allow them — they are intentional public API for tests and future callers.
#![allow(dead_code)]

mod adapters;
mod config;
mod index;
mod query;
mod search;
mod session;
mod tui;
mod util;

use std::collections::HashMap;
use std::time::Instant;

use clap::{Parser, Subcommand};

use search::SessionSearch;
use tui::{app::TuiOpts, run_tui};

/// fast-resume: search and resume AI coding agent sessions.
#[derive(Parser)]
#[command(name = "fr", version)]
struct Cli {
    /// Pre-fill the search box with this query.
    #[arg(value_name = "QUERY")]
    query: Option<String>,

    /// Force Unicode half-block rendering instead of Sixel/Kitty inline images.
    ///
    /// Use this flag on terminals that don't support inline graphics, in CI, or
    /// when capturing screenshots where deterministic output is required.
    #[arg(long)]
    no_images: bool,

    /// Skip the yolo confirmation modal and always pass auto-approve flags.
    #[arg(long)]
    yolo: bool,

    /// Print sessions as a table to stdout without launching the TUI.
    #[arg(long)]
    no_tui: bool,

    /// Print index statistics (per-agent session counts and adapter info).
    #[arg(long)]
    stats: bool,

    /// Wipe the index and force a full re-index on next run.
    #[arg(long)]
    rebuild: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan all agent directories, index sessions, and report statistics.
    Index,
}

fn main() {
    let cli = Cli::parse();

    if cli.rebuild {
        cmd_rebuild();
        return;
    }

    if cli.stats {
        cmd_stats();
        return;
    }

    match cli.command {
        Some(Commands::Index) => cmd_index(),
        None => {
            if cli.no_tui {
                cmd_no_tui(cli.query.as_deref().unwrap_or(""));
            } else {
                cmd_tui(cli.query.as_deref().unwrap_or(""), cli.no_images, cli.yolo);
            }
        }
    }
}

/// `fr index` — scan + index all sessions and print stats.
fn cmd_index() {
    let started = Instant::now();
    let search = SessionSearch::new();

    println!("Scanning sessions…");
    let sessions = match search.get_all_sessions() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error indexing sessions: {}", e);
            std::process::exit(1);
        }
    };

    let elapsed = started.elapsed();

    let mut per_adapter: HashMap<String, usize> = HashMap::new();
    for s in &sessions {
        *per_adapter.entry(s.agent.clone()).or_insert(0) += 1;
    }

    let adapter_count = search.adapter_count();
    println!(
        "Indexed {} sessions across {} adapters ({:.2}s)",
        sessions.len(),
        adapter_count,
        elapsed.as_secs_f64()
    );

    let mut agents: Vec<(&String, &usize)> = per_adapter.iter().collect();
    agents.sort_by_key(|(name, _)| name.as_str());
    for (agent, count) in agents {
        println!("  {:<16} {}", agent, count);
    }
}

/// `fr --stats` — print index statistics.
fn cmd_stats() {
    let search = SessionSearch::new();

    let sessions = match search.get_all_sessions() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error loading sessions: {}", e);
            std::process::exit(1);
        }
    };

    let mut per_agent: HashMap<String, usize> = HashMap::new();
    for s in &sessions {
        *per_agent.entry(s.agent.clone()).or_insert(0) += 1;
    }

    println!("Total sessions indexed: {}", sessions.len());
    println!();

    // Per-adapter raw stats.
    let raw_stats = search.get_raw_stats();
    println!("{:<18} {:>8}  {:>12}  {:>8}  Available", "Agent", "Sessions", "Size", "Files");
    println!("{}", "-".repeat(65));
    for rs in &raw_stats {
        let count = per_agent.get(&rs.agent).copied().unwrap_or(0);
        let size = humanize_bytes(rs.total_bytes);
        let avail = if rs.available { "yes" } else { "no" };
        println!(
            "{:<18} {:>8}  {:>12}  {:>8}  {}",
            rs.agent, count, size, rs.file_count, avail
        );
    }
}

/// `fr --no-tui [QUERY]` — list sessions as a table.
fn cmd_no_tui(query: &str) {
    let search = SessionSearch::new();

    let sessions = if query.is_empty() {
        match search.get_all_sessions() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error loading sessions: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match search.get_all_sessions().and_then(|_| search.search(query, 100)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error searching sessions: {}", e);
                std::process::exit(1);
            }
        }
    };

    if sessions.is_empty() {
        println!("No sessions found.");
        return;
    }

    // Print table header.
    println!(
        "{:<10}  {:<36}  {:<40}  {:>5}  Directory",
        "Agent", "ID", "Title", "Msgs"
    );
    println!("{}", "-".repeat(140));

    for s in &sessions {
        let short_id = if s.id.len() > 34 {
            format!("{}…", &s.id[..34])
        } else {
            s.id.clone()
        };
        let short_title = if s.title.len() > 38 {
            format!("{}…", &s.title[..38])
        } else {
            s.title.clone()
        };
        println!(
            "{:<10}  {:<36}  {:<40}  {:>5}  {}",
            s.agent, short_id, short_title, s.message_count, s.directory
        );
    }
}

/// `fr --rebuild` — wipe the index and rebuild from scratch.
fn cmd_rebuild() {
    use crate::index::TantivyIndex;
    use crate::config;

    println!("Wiping index…");
    let index = TantivyIndex::new(config::index_dir());
    if let Err(e) = index.clear() {
        eprintln!("Failed to clear index: {}", e);
        std::process::exit(1);
    }
    println!("Index cleared. Running full re-index…");
    cmd_index();
}

/// Default command — launch the TUI.
fn cmd_tui(initial_query: &str, no_images: bool, yolo: bool) {
    let opts = TuiOpts {
        initial_query,
        no_images,
        yolo,
    };
    match run_tui(opts) {
        Ok(result) => {
            if let (Some(cmd), Some(dir)) = (result.resume_command, result.resume_dir) {
                // ratatui::restore() has already been called inside run_tui().
                println!("Launching: {}", cmd.join(" "));

                // Verify the binary exists on PATH before attempting exec.
                if let Err(e) = which::which(&cmd[0]) {
                    eprintln!("error: '{}' not found on PATH: {}", cmd[0], e);
                    std::process::exit(1);
                }

                let err = util::proc::replace_process(&cmd, Some(std::path::Path::new(&dir)));
                eprintln!("error: failed to launch '{}': {}", cmd[0], err);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("TUI error: {e}");
            std::process::exit(1);
        }
    }
}

/// Simple byte size formatter.
fn humanize_bytes(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * KB;
    const GB: u64 = 1_024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
