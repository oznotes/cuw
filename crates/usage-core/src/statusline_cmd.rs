// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! The `--statusline` helper. When registered as Claude Code's
//! `statusLine.command`, this receives the session JSON on stdin after each
//! assistant message. We extract `rate_limits` into the cache (so the widget
//! gets fresh quota with no network) and print a minimal status line.

use std::path::Path;

use crate::sources::statusline;

/// Update the cache (best-effort) and return the status-line text to print.
pub fn run_statusline(stdin_json: &str) -> String {
    run_statusline_to(stdin_json, &statusline::cache_path())
}

/// Testable core with an explicit cache path.
pub fn run_statusline_to(stdin_json: &str, cache_path: &Path) -> String {
    // Never fail the status line because of a cache write problem.
    let _ = statusline::write_cache_from_stdin(stdin_json, cache_path);
    format_line(stdin_json)
}

fn format_line(stdin_json: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdin_json) else {
        return String::new();
    };
    let model = v
        .get("model")
        .and_then(|m| m.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or("claude");
    let pct = |name: &str| {
        v.get("rate_limits")
            .and_then(|r| r.get(name))
            .and_then(|w| w.get("used_percentage"))
            .and_then(|x| x.as_f64())
    };
    match (pct("five_hour"), pct("seven_day")) {
        (Some(h), Some(d)) => format!("{model} · 5h {h:.0}% · 7d {d:.0}%"),
        _ => model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_model_and_percentages() {
        let stdin = r#"{"model":{"id":"claude-opus-4-8"},
            "rate_limits":{"five_hour":{"used_percentage":72.4},"seven_day":{"used_percentage":41}}}"#;
        assert_eq!(format_line(stdin), "claude-opus-4-8 · 5h 72% · 7d 41%");
    }

    #[test]
    fn formats_model_only_without_rate_limits() {
        let stdin = r#"{"model":{"id":"claude-opus-4-8"}}"#;
        assert_eq!(format_line(stdin), "claude-opus-4-8");
    }

    #[test]
    fn empty_on_garbage() {
        assert_eq!(format_line("not json"), "");
    }

    #[test]
    fn writes_cache_as_side_effect() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ratelimits.json");
        let stdin = r#"{"model":{"id":"claude-opus-4-8"},
            "rate_limits":{"five_hour":{"used_percentage":50,"resets_at":1780012345},
                           "seven_day":{"used_percentage":10,"resets_at":1780500000}}}"#;
        let line = run_statusline_to(stdin, &p);
        assert!(line.contains("5h 50%"));
        assert!(statusline::read_cache(&p).is_some());
    }
}
