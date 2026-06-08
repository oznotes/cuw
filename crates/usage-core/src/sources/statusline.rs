//! The local status-line quota cache.
//!
//! Claude Code pipes a JSON blob (including `rate_limits`) to its configured
//! `statusLine.command` after each assistant message. Our `--statusline` helper
//! ([`crate::statusline_cmd`]) extracts that and writes a small cache file here;
//! the widget reads it with zero network cost.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::QuotaReading;
use crate::model::{Provenance, Window};
use crate::timeutil::{unix_millis_to_systemtime, unix_secs_to_systemtime};

/// One window in our cache file. `resets_at` is Unix **seconds** (as the
/// status-line feed provides).
#[derive(Serialize, Deserialize)]
struct CacheWindow {
    used_percentage: f64,
    #[serde(default)]
    resets_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    observed_unix_ms: i64,
    five_hour: CacheWindow,
    seven_day: CacheWindow,
}

/// `~/.claude/widget-cache/ratelimits.json`.
pub fn cache_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("widget-cache")
        .join("ratelimits.json")
}

/// Read the cache. Returns `None` on any problem (missing / unreadable / corrupt).
pub fn read_cache(path: &Path) -> Option<QuotaReading> {
    let s = std::fs::read_to_string(path).ok()?;
    let c: Cache = serde_json::from_str(&s).ok()?;
    let mk = |w: &CacheWindow| Window {
        used_pct: w.used_percentage as f32,
        resets_at: w.resets_at.map(unix_secs_to_systemtime),
    };
    Some(QuotaReading {
        five_hour: mk(&c.five_hour),
        seven_day: mk(&c.seven_day),
        seven_day_opus: None,
        source: Provenance::StatusLine,
        observed_at: unix_millis_to_systemtime(c.observed_unix_ms),
    })
}

/// Parse Claude Code's status-line stdin JSON and write our cache file. Uses the
/// wall clock for the observed time.
pub fn write_cache_from_stdin(stdin_json: &str, path: &Path) -> Result<()> {
    write_cache_from_stdin_at(stdin_json, path, SystemTime::now())
}

/// Testable core of [`write_cache_from_stdin`] with an injected `now`. If the
/// stdin JSON carries no usable `rate_limits`, this is a no-op (we never clobber
/// a good cache with nothing).
pub fn write_cache_from_stdin_at(stdin_json: &str, path: &Path, now: SystemTime) -> Result<()> {
    let v: serde_json::Value =
        serde_json::from_str(stdin_json).context("parsing statusline stdin JSON")?;
    let Some(rl) = v.get("rate_limits") else {
        return Ok(());
    };
    let win = |name: &str| -> Option<CacheWindow> {
        let o = rl.get(name)?;
        let used = o.get("used_percentage")?.as_f64()?;
        let resets_at = o.get("resets_at").and_then(|r| r.as_i64());
        Some(CacheWindow {
            used_percentage: used,
            resets_at,
        })
    };
    let (Some(five_hour), Some(seven_day)) = (win("five_hour"), win("seven_day")) else {
        return Ok(());
    };

    let observed_unix_ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let cache = Cache {
        observed_unix_ms,
        five_hour,
        seven_day,
    };
    let json = serde_json::to_string(&cache)?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json.as_bytes()).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("widget-cache").join("ratelimits.json");
        let stdin = r#"{"rate_limits":{
            "five_hour":{"used_percentage":72,"resets_at":1780012345},
            "seven_day":{"used_percentage":41.5,"resets_at":1780500000}},
            "model":{"id":"claude-opus-4-8"}}"#;
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_780_000_000);

        write_cache_from_stdin_at(stdin, &p, now).unwrap();
        assert!(p.exists());

        let r = read_cache(&p).unwrap();
        assert_eq!(r.five_hour.used_pct, 72.0);
        assert_eq!(r.seven_day.used_pct, 41.5);
        assert_eq!(r.source, Provenance::StatusLine);
        assert_eq!(r.observed_at, now); // whole seconds => exact ms round-trip
        assert!(r.five_hour.resets_at.is_some());
    }

    #[test]
    fn no_rate_limits_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ratelimits.json");
        write_cache_from_stdin_at(r#"{"model":{"id":"x"}}"#, &p, SystemTime::now()).unwrap();
        assert!(!p.exists());
        assert!(read_cache(&p).is_none());
    }

    #[test]
    fn partial_rate_limits_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ratelimits.json");
        // only five_hour present; seven_day missing => skip
        let stdin = r#"{"rate_limits":{"five_hour":{"used_percentage":10}}}"#;
        write_cache_from_stdin_at(stdin, &p, SystemTime::now()).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn read_missing_or_corrupt_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ratelimits.json");
        assert!(read_cache(&p).is_none());
        std::fs::write(&p, b"not json at all").unwrap();
        assert!(read_cache(&p).is_none());
    }
}
