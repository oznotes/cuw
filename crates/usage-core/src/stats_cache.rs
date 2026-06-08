//! Historical daily token totals from `~/.claude/stats-cache.json`, and a
//! GitHub-contributions-style heatmap built from them.
//!
//! `stats-cache.json` is Claude Code's own pre-aggregated history; its
//! `dailyModelTokens` array gives per-day tokens-by-model. It lags real time
//! (recomputed periodically) which is fine for a slow-moving activity grid.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Duration, NaiveDate};

/// `~/.claude/stats-cache.json`.
pub fn default_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("stats-cache.json")
}

/// Map of `YYYY-MM-DD` → total tokens that day (summed across models).
/// Empty on any read/parse problem.
pub fn read_daily_tokens(path: &Path) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    let Ok(s) = std::fs::read_to_string(path) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return out;
    };
    if let Some(arr) = v.get("dailyModelTokens").and_then(|x| x.as_array()) {
        for entry in arr {
            let Some(date) = entry.get("date").and_then(|d| d.as_str()) else {
                continue;
            };
            let total: u64 = entry
                .get("tokensByModel")
                .and_then(|m| m.as_object())
                .map(|m| m.values().filter_map(|x| x.as_u64()).sum())
                .unwrap_or(0);
            *out.entry(date.to_string()).or_insert(0) += total;
        }
    }
    out
}

/// Quantize a day's token count to an intensity level 0..=4 (GitHub-style).
pub fn level_for(tokens: u64) -> u8 {
    match tokens {
        0 => 0,
        1..=49_999 => 1,
        50_000..=149_999 => 2,
        150_000..=349_999 => 3,
        _ => 4,
    }
}

/// Build the grid: `weeks` columns (oldest → newest), each a `[u8; 7]` of
/// intensity levels for Sunday..Saturday. The last column contains `today`.
/// Future cells (after `today`) are level 0.
pub fn build_heatmap(daily: &HashMap<String, u64>, today: NaiveDate, weeks: usize) -> Vec<[u8; 7]> {
    let today_dow = today.weekday().num_days_from_sunday() as i64; // Sun=0..Sat=6
    let last_sunday = today - Duration::days(today_dow);
    let first_sunday = last_sunday - Duration::weeks(weeks.saturating_sub(1) as i64);

    let mut grid = Vec::with_capacity(weeks);
    for w in 0..weeks {
        let mut col = [0u8; 7];
        for (d, cell) in col.iter_mut().enumerate() {
            let date = first_sunday + Duration::weeks(w as i64) + Duration::days(d as i64);
            if date > today {
                continue; // future
            }
            let key = date.format("%Y-%m-%d").to_string();
            *cell = level_for(daily.get(&key).copied().unwrap_or(0));
        }
        grid.push(col);
    }
    grid
}

/// Convert a `SystemTime` to a UTC `NaiveDate`.
fn utc_date(now: SystemTime) -> NaiveDate {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.date_naive())
        .unwrap_or_default()
}

/// `build_heatmap` keyed off a `SystemTime` instead of a `NaiveDate`.
pub fn build_heatmap_at(
    daily: &HashMap<String, u64>,
    now: SystemTime,
    weeks: usize,
) -> Vec<[u8; 7]> {
    build_heatmap(daily, utc_date(now), weeks)
}

/// Convenience: read the default stats-cache and build a `weeks`-wide heatmap
/// as of `now`.
pub fn heatmap(now: SystemTime, weeks: usize) -> Vec<[u8; 7]> {
    let daily = read_daily_tokens(&default_path());
    build_heatmap_at(&daily, now, weeks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_sums_daily_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("stats-cache.json");
        std::fs::write(
            &p,
            r#"{"dailyModelTokens":[
                {"date":"2026-06-01","tokensByModel":{"claude-opus-4-8":100,"claude-sonnet-4-6":50}},
                {"date":"2026-06-02","tokensByModel":{"claude-opus-4-8":200000}}
            ]}"#,
        )
        .unwrap();
        let m = read_daily_tokens(&p);
        assert_eq!(m.get("2026-06-01"), Some(&150));
        assert_eq!(m.get("2026-06-02"), Some(&200000));
    }

    #[test]
    fn read_missing_is_empty() {
        assert!(read_daily_tokens(Path::new("Z:/nope/stats-cache.json")).is_empty());
    }

    #[test]
    fn levels_quantize() {
        assert_eq!(level_for(0), 0);
        assert_eq!(level_for(1), 1);
        assert_eq!(level_for(49_999), 1);
        assert_eq!(level_for(50_000), 2);
        assert_eq!(level_for(150_000), 3);
        assert_eq!(level_for(999_999), 4);
    }

    #[test]
    fn heatmap_dims_and_today_placement() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 8).unwrap(); // a Monday
        let mut daily = HashMap::new();
        daily.insert("2026-06-08".to_string(), 200_000u64); // today => level 3
        let grid = build_heatmap(&daily, today, 12);
        assert_eq!(grid.len(), 12); // 12 week-columns
        // today is Monday => num_days_from_sunday = 1; it's in the LAST column, row 1.
        assert_eq!(grid[11][1], 3);
        // a future day in the last column (e.g. Saturday row 6) must be 0.
        assert_eq!(grid[11][6], 0);
    }

    #[test]
    fn heatmap_empty_history_all_zero() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 8).unwrap();
        let grid = build_heatmap(&HashMap::new(), today, 8);
        assert!(grid.iter().all(|col| col.iter().all(|&c| c == 0)));
    }
}
