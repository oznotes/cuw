//! Orchestrates the three sources into one [`UsageSnapshot`] per tick.
//!
//! Quota policy: prefer a **fresh** status-line cache (free); otherwise poll the
//! OAuth endpoint, but no more often than `quota_poll_secs`; otherwise degrade
//! to the last good reading. The OAuth fetch is injected so the policy is
//! testable without a network.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::model::{Provenance, UsageSnapshot, Window};
use crate::sources::jsonl::{Cursor, TokenLedger};
use crate::sources::{QuotaReading, reconcile, statusline};

/// Returns a fresh OAuth reading (stamped at `now`) or `None` on failure.
pub type OauthFetch = Box<dyn FnMut(SystemTime) -> Option<QuotaReading> + Send>;

pub struct Collector {
    cursor: Cursor,
    ledger: TokenLedger,
    last_good: Option<QuotaReading>,
    last_oauth_at: Option<SystemTime>,
    config: Config,
    projects_root: PathBuf,
    statusline_cache: PathBuf,
    oauth_fetch: OauthFetch,
}

impl Collector {
    /// Wire up the real `~/.claude` paths and the default fetcher (a no-op
    /// unless the `net` feature is enabled).
    pub fn new(config: Config) -> Self {
        let claude = dirs::home_dir().unwrap_or_default().join(".claude");
        let projects_root = claude.join("projects");
        let statusline_cache = statusline::cache_path();
        Self::with_deps(
            config,
            projects_root,
            statusline_cache,
            default_oauth_fetch(),
        )
    }

    /// Construct with explicit paths + fetcher (used by tests).
    pub fn with_deps(
        config: Config,
        projects_root: PathBuf,
        statusline_cache: PathBuf,
        oauth_fetch: OauthFetch,
    ) -> Self {
        Collector {
            cursor: Cursor::new(),
            ledger: TokenLedger::new(),
            last_good: None,
            last_oauth_at: None,
            config,
            projects_root,
            statusline_cache,
            oauth_fetch,
        }
    }

    /// One refresh.
    pub fn tick(&mut self, now: SystemTime) -> UsageSnapshot {
        let max_age = Duration::from_secs(self.config.statusline_max_age_secs);

        // 1. Local token detail (incremental).
        let _ = self
            .cursor
            .update(&self.projects_root, &mut self.ledger, now);
        let tokens = self.ledger.stats(now);

        // 2. Status-line cache.
        let sl = statusline::read_cache(&self.statusline_cache);
        let sl_fresh = sl
            .as_ref()
            .map(|r| {
                now.duration_since(r.observed_at)
                    .map(|d| d <= max_age)
                    .unwrap_or(true)
            })
            .unwrap_or(false);

        // 3. Poll OAuth only if the status line isn't fresh AND the throttle elapsed.
        let throttle_ok = match self.last_oauth_at {
            None => true,
            Some(t) => now
                .duration_since(t)
                .map(|d| d >= Duration::from_secs(self.config.quota_poll_secs))
                .unwrap_or(true),
        };
        let oauth = if !sl_fresh && throttle_ok {
            self.last_oauth_at = Some(now);
            (self.oauth_fetch)(now)
        } else {
            None
        };

        // 4. Reconcile.
        let chosen = reconcile(sl, oauth, self.last_good.clone(), now, max_age);

        // 5. Build the snapshot.
        match chosen {
            Some((reading, prov)) => {
                if matches!(prov, Provenance::StatusLine | Provenance::OAuth) {
                    self.last_good = Some(reading.clone());
                }
                UsageSnapshot {
                    five_hour: reading.five_hour,
                    seven_day: reading.seven_day,
                    seven_day_opus: reading.seven_day_opus,
                    tokens,
                    source: prov,
                    fetched_at: now,
                }
            }
            None => UsageSnapshot {
                five_hour: Window {
                    used_pct: 0.0,
                    resets_at: None,
                },
                seven_day: Window {
                    used_pct: 0.0,
                    resets_at: None,
                },
                seven_day_opus: None,
                tokens,
                source: Provenance::Stale { last_good_at: now },
                fetched_at: now,
            },
        }
    }
}

#[cfg(not(feature = "net"))]
fn default_oauth_fetch() -> OauthFetch {
    Box::new(|_now| None)
}

#[cfg(feature = "net")]
fn default_oauth_fetch() -> OauthFetch {
    Box::new(|now| {
        let token = read_access_token()?;
        let version = detect_cc_version();
        crate::sources::oauth::fetch(&token, &version)
            .ok()
            .map(|mut r| {
                r.observed_at = now;
                r
            })
    })
}

/// Best-effort Claude Code version for the mandatory OAuth `User-Agent`.
/// Env override wins; otherwise read the newest `~/.claude/sessions/*.json`
/// `version` field; otherwise a recent fallback.
#[cfg(feature = "net")]
fn detect_cc_version() -> String {
    if let Ok(v) = std::env::var("CLAUDE_CODE_VERSION") {
        return v;
    }
    if let Some(home) = dirs::home_dir() {
        let dir = home.join(".claude").join("sessions");
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut best: Option<(std::time::SystemTime, String)> = None;
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let mtime = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                if let Ok(s) = std::fs::read_to_string(&p) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                        if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
                            if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                                best = Some((mtime, ver.to_string()));
                            }
                        }
                    }
                }
            }
            if let Some((_, v)) = best {
                return v;
            }
        }
    }
    "2.1.0".to_string()
}

/// Read `claudeAiOauth.accessToken` from `~/.claude/.credentials.json`.
#[cfg(feature = "net")]
fn read_access_token() -> Option<String> {
    let path = dirs::home_dir()?.join(".claude").join(".credentials.json");
    let s = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Window;
    use crate::timeutil::iso8601_to_systemtime;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn cfg() -> Config {
        Config::default()
    }

    fn oauth_reading(pct: f32, now: SystemTime) -> QuotaReading {
        QuotaReading {
            five_hour: Window {
                used_pct: pct,
                resets_at: None,
            },
            seven_day: Window {
                used_pct: pct / 2.0,
                resets_at: None,
            },
            seven_day_opus: None,
            source: Provenance::OAuth,
            observed_at: now,
        }
    }

    /// A fetcher that records how many times it was called and returns `ret`.
    fn counting_fetch(ret: Option<f32>) -> (OauthFetch, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let f: OauthFetch = Box::new(move |now| {
            c.fetch_add(1, Ordering::SeqCst);
            ret.map(|p| oauth_reading(p, now))
        });
        (f, calls)
    }

    fn empty_projects() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        (dir, root)
    }

    #[test]
    fn fresh_statusline_skips_oauth() {
        let (_d, root) = empty_projects();
        let sl_dir = tempfile::tempdir().unwrap();
        let sl_path = sl_dir.path().join("ratelimits.json");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_780_000_000);
        statusline::write_cache_from_stdin_at(
            r#"{"rate_limits":{"five_hour":{"used_percentage":55},"seven_day":{"used_percentage":20}}}"#,
            &sl_path,
            now,
        )
        .unwrap();

        let (fetch, calls) = counting_fetch(Some(99.0));
        let mut c = Collector::with_deps(cfg(), root, sl_path, fetch);
        let snap = c.tick(now);

        assert_eq!(snap.source, Provenance::StatusLine);
        assert_eq!(snap.five_hour.used_pct, 55.0);
        assert_eq!(calls.load(Ordering::SeqCst), 0); // oauth not polled
    }

    #[test]
    fn polls_oauth_when_no_statusline_then_throttles() {
        let (_d, root) = empty_projects();
        let sl_path = PathBuf::from("Z:/nonexistent/ratelimits.json");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_780_000_000);

        let (fetch, calls) = counting_fetch(Some(60.0));
        let mut c = Collector::with_deps(cfg(), root, sl_path, fetch);

        let s1 = c.tick(now);
        assert_eq!(s1.source, Provenance::OAuth);
        assert_eq!(s1.five_hour.used_pct, 60.0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // immediate second tick: throttle blocks the poll; falls back to last_good as Stale
        let s2 = c.tick(now + Duration::from_secs(5));
        assert_eq!(calls.load(Ordering::SeqCst), 1); // not polled again
        assert!(matches!(s2.source, Provenance::Stale { .. }));
        assert_eq!(s2.five_hour.used_pct, 60.0); // last good reused

        // after the throttle window, it polls again
        let s3 = c.tick(now + Duration::from_secs(200));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(s3.source, Provenance::OAuth);
    }

    #[test]
    fn nothing_available_yields_zeroed_stale() {
        let (_d, root) = empty_projects();
        let sl_path = PathBuf::from("Z:/nope/ratelimits.json");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_780_000_000);
        let (fetch, _calls) = counting_fetch(None); // oauth fails
        let mut c = Collector::with_deps(cfg(), root, sl_path, fetch);

        let snap = c.tick(now);
        assert!(matches!(snap.source, Provenance::Stale { .. }));
        assert_eq!(snap.five_hour.used_pct, 0.0);
        assert_eq!(snap.seven_day.used_pct, 0.0);
    }

    #[test]
    fn token_detail_comes_from_jsonl() {
        let (_d, root) = empty_projects();
        let slug = root.join("projA");
        std::fs::create_dir_all(&slug).unwrap();
        let l = |msg: &str, req: &str, out: u64| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-06-08T11:00:00Z","requestId":"{req}","message":{{"id":"{msg}","model":"opus","usage":{{"output_tokens":{out}}}}}}}"#
            )
        };
        std::fs::write(
            slug.join("s.jsonl"),
            format!("{}\n{}\n", l("m1", "r1", 100), l("m2", "r2", 250)),
        )
        .unwrap();

        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let sl_path = PathBuf::from("Z:/nope/ratelimits.json");
        let (fetch, _c) = counting_fetch(Some(10.0));
        let mut c = Collector::with_deps(cfg(), root, sl_path, fetch);

        let snap = c.tick(now);
        assert_eq!(snap.tokens.today_total_output, 350);
    }
}
