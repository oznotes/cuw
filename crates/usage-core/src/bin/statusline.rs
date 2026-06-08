//! `claude-usage-statusline` — register this as Claude Code's
//! `statusLine.command` in `~/.claude/settings.json`:
//!
//! ```json
//! { "statusLine": { "type": "command",
//!     "command": "C:\\path\\to\\claude-usage-statusline.exe" } }
//! ```
//!
//! Claude Code pipes the session JSON (including `rate_limits`) to stdin after
//! each assistant message. We cache the rate limits for the widget and echo a
//! one-line status. Needs no gpui — builds with the stable toolchain alone.

use std::io::Read;

fn main() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let line = usage_core::statusline_cmd::run_statusline(&input);
    println!("{line}");
}
