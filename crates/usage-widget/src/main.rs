//! `claude-usage-widget` entry point.
//!
//! Two modes:
//! - `--statusline` : status-line helper (no GUI). Lets this single binary be
//!   registered as Claude Code's `statusLine.command` if you'd rather not build
//!   the separate `claude-usage-statusline`. Pure `usage-core`, no gpui.
//! - default        : launch the always-on-top frosted-glass widget.

mod ui;
mod win;

fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--statusline") {
        use std::io::Read;
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).ok();
        println!("{}", usage_core::statusline_cmd::run_statusline(&input));
        return Ok(());
    }

    let config = usage_core::config::Config::load();
    ui::run(config)
}
