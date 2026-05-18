mod adapters;
mod config;
mod index;
mod search;
mod session;

use std::time::Instant;

use clap::{Parser, Subcommand};

use adapters::claude::ClaudeAdapter;
use adapters::AgentAdapter;
use search::SessionSearch;

/// fast-resume: search and resume AI coding agent sessions.
#[derive(Parser)]
#[command(name = "fr", version)]
struct Cli {
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

    match cli.command {
        Some(Commands::Index) => cmd_index(),
        None => cmd_list(),
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

    // Count per adapter.
    let mut per_adapter: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
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

/// Default command (no subcommand) — list Claude sessions as before.
fn cmd_list() {
    let adapter = ClaudeAdapter::new();
    if !adapter.is_available() {
        eprintln!("~/.claude/projects/ not found — no Claude sessions to list");
        return;
    }

    let sessions = adapter.find_sessions();
    println!("Found {} Claude session(s):", sessions.len());
    for s in sessions.iter().take(10) {
        println!("  [{}] {} — {}", s.agent, s.title, s.directory);
    }
}
