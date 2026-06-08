//! Print one UsageSnapshot from the real local `~/.claude` data.
//!
//! Run (local only, no network):   cargo run -p usage-core --example snapshot
//! With the live quota fetch:       cargo run -p usage-core --example snapshot --features net
//!
//! Without `net` (and with no status-line cache yet) the quota windows read as
//! a zeroed `Stale` snapshot — that is expected; the token detail below it is
//! computed from your actual JSONL transcripts.

use std::time::SystemTime;

use usage_core::collector::Collector;
use usage_core::config::Config;

fn main() {
    let mut collector = Collector::new(Config::default());
    let snap = collector.tick(SystemTime::now());

    println!("=== Claude usage snapshot ===");
    println!("quota source : {:?}", snap.source);
    println!("5-hour       : {:.1}%", snap.five_hour.used_pct);
    println!("7-day        : {:.1}%", snap.seven_day.used_pct);
    if let Some(opus) = &snap.seven_day_opus {
        println!("7-day (opus) : {:.1}%", opus.used_pct);
    }
    println!("--- local token detail (today, UTC) ---");
    println!("output tokens: {}", snap.tokens.today_total_output);
    for (model, toks) in &snap.tokens.by_model {
        println!("  {model:<28} {toks:>12}");
    }
    if let Some(rate) = snap.tokens.live_tok_per_min {
        println!("live rate    : {rate:.0} tok/min");
    }
    if !snap.tokens.top_projects.is_empty() {
        println!("top projects :");
        for (p, t) in snap.tokens.top_projects.iter().take(5) {
            println!("  {p:<40} {t:>12}");
        }
    }
}
