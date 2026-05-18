mod adapters;
mod config;
mod session;

use adapters::claude::ClaudeAdapter;
use adapters::AgentAdapter;
use clap::Parser;

/// fast-resume: search and resume AI coding agent sessions.
#[derive(Parser)]
#[command(name = "fr", version)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();

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
