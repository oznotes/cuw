//! Local token detail from `~/.claude/projects/*/*.jsonl`.
//!
//! Three pieces:
//! - [`parse_line`] — one JSONL line → an [`AssistantRecord`] (or nothing).
//! - [`TokenLedger`] — dedup, UTC-day bucketing, live token rate.
//! - [`Cursor`] — incremental reader that only consumes newly appended bytes,
//!   so a refresh touches kilobytes, never the whole ~107 MB history.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::model::TokenStats;
use crate::timeutil::{iso8601_to_systemtime, utc_day};

/// Output tokens within this window of `now` feed the "live tok/min" figure.
const LIVE_WINDOW: Duration = Duration::from_secs(90);
/// Records older than this (relative to `now`) are not kept in the live ring.
const RING_KEEP: Duration = Duration::from_secs(300);

/// One assistant turn's usage, extracted from a JSONL line.
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantRecord {
    pub message_id: String,
    pub request_id: String,
    pub model: String,
    pub output_tokens: u64,
    pub timestamp: SystemTime,
    /// Parent directory name of the source jsonl file.
    pub project: String,
}

/// Parse one JSONL line. Returns `Ok(None)` for any non-assistant-usage line
/// (user turns, summaries, assistant turns without a usable `output_tokens`,
/// blank lines); `Err` only for malformed JSON.
pub fn parse_line(line: &str, project: &str) -> Result<Option<AssistantRecord>> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_str(line).context("parsing jsonl line")?;
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Ok(None);
    }
    let Some(msg) = v.get("message") else {
        return Ok(None);
    };
    let Some(usage) = msg.get("usage") else {
        return Ok(None);
    };
    let Some(output_tokens) = usage.get("output_tokens").and_then(|x| x.as_u64()) else {
        return Ok(None);
    };
    let Some(ts_str) = v.get("timestamp").and_then(|t| t.as_str()) else {
        return Ok(None);
    };
    let timestamp = iso8601_to_systemtime(ts_str)?;

    Ok(Some(AssistantRecord {
        message_id: msg
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        request_id: v
            .get("requestId")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        model: msg
            .get("model")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        output_tokens,
        timestamp,
        project: project.to_string(),
    }))
}

/// Accumulates deduped assistant records into today's (UTC) totals + a live rate.
pub struct TokenLedger {
    seen: HashSet<(String, String)>,
    /// UTC-day index the `today` maps belong to (`i64::MIN` until first ingest).
    day: i64,
    by_model_today: HashMap<String, u64>,
    by_project_today: HashMap<String, u64>,
    /// All-time output tokens per UTC-day index, for the activity heatmap.
    /// (Not cleared on day rolls — this is the full history.)
    by_day: HashMap<i64, u64>,
    recent: VecDeque<(SystemTime, u64)>,
}

impl Default for TokenLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenLedger {
    pub fn new() -> Self {
        TokenLedger {
            seen: HashSet::new(),
            day: i64::MIN,
            by_model_today: HashMap::new(),
            by_project_today: HashMap::new(),
            by_day: HashMap::new(),
            recent: VecDeque::new(),
        }
    }

    fn roll_to(&mut self, now: SystemTime) {
        let d = utc_day(now);
        if d != self.day {
            self.day = d;
            self.by_model_today.clear();
            self.by_project_today.clear();
        }
    }

    /// Fold a record into the ledger. Dedupes on `(message_id, request_id)`
    /// (records with both empty are always counted — they cannot be deduped).
    /// Only records whose timestamp is the current UTC day contribute to today.
    pub fn ingest(&mut self, rec: &AssistantRecord, now: SystemTime) {
        self.roll_to(now);

        if !(rec.message_id.is_empty() && rec.request_id.is_empty()) {
            let key = (rec.message_id.clone(), rec.request_id.clone());
            if !self.seen.insert(key) {
                return; // duplicate streaming row — skip
            }
        }

        // All-time per-day total (for the activity heatmap; survives day rolls).
        *self.by_day.entry(utc_day(rec.timestamp)).or_default() += rec.output_tokens;

        if utc_day(rec.timestamp) == self.day {
            *self.by_model_today.entry(rec.model.clone()).or_default() += rec.output_tokens;
            *self
                .by_project_today
                .entry(rec.project.clone())
                .or_default() += rec.output_tokens;
        }

        let recent_enough = now
            .duration_since(rec.timestamp)
            .map(|d| d <= RING_KEEP)
            .unwrap_or(true); // future timestamp => keep
        if recent_enough {
            self.recent.push_back((rec.timestamp, rec.output_tokens));
        }
        while let Some(&(ts, _)) = self.recent.front() {
            if now
                .duration_since(ts)
                .map(|d| d > RING_KEEP)
                .unwrap_or(false)
            {
                self.recent.pop_front();
            } else {
                break;
            }
        }
    }

    /// Snapshot today's stats. If `now` is no longer the ledger's bucketed day,
    /// today's totals read as zero (the bucket belongs to a past day).
    pub fn stats(&self, now: SystemTime) -> TokenStats {
        let today_valid = utc_day(now) == self.day;

        let mut by_model: Vec<(String, u64)> = if today_valid {
            self.by_model_today
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect()
        } else {
            Vec::new()
        };
        by_model.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut top_projects: Vec<(String, u64)> = if today_valid {
            self.by_project_today
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect()
        } else {
            Vec::new()
        };
        top_projects.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let today_total_output = by_model.iter().map(|(_, v)| *v).sum();

        let live_sum: u64 = self
            .recent
            .iter()
            .filter(|(ts, _)| {
                now.duration_since(*ts)
                    .map(|d| d <= LIVE_WINDOW)
                    .unwrap_or(false)
            })
            .map(|(_, v)| *v)
            .sum();
        let live_tok_per_min = if live_sum > 0 {
            Some(live_sum as f64 / (LIVE_WINDOW.as_secs_f64() / 60.0))
        } else {
            None
        };

        TokenStats {
            today_total_output,
            by_model,
            live_tok_per_min,
            top_projects,
            activity_heatmap: Vec::new(),
        }
    }

    /// GitHub-style activity grid from the all-time JSONL day totals (current
    /// data, unlike the periodically-recomputed stats-cache). Shows at most
    /// `max_weeks` columns, but **trims leading empty weeks** so the first
    /// column is the earliest active week — no blank left half. Each column is
    /// `[u8; 7]` of levels for Sunday..Saturday; the last column contains `now`.
    pub fn activity_heatmap(&self, now: SystemTime, max_weeks: usize) -> Vec<[u8; 7]> {
        let today = utc_day(now);
        let max_weeks = max_weeks.max(1) as i64;
        // 1970-01-01 (day index 0) was a Thursday; days-from-Sunday = (d + 4) mod 7.
        let last_sunday = today - (today + 4).rem_euclid(7);
        let window_start = last_sunday - 7 * (max_weeks - 1);

        // Start the grid at the week of the earliest active day in the window.
        let first_sunday = self
            .by_day
            .iter()
            .filter_map(|(&d, &t)| (t > 0 && d >= window_start && d <= today).then_some(d))
            .min()
            .map(|d| d - (d + 4).rem_euclid(7))
            .unwrap_or(last_sunday);
        let weeks = (((last_sunday - first_sunday) / 7) + 1) as usize;

        let mut grid = Vec::with_capacity(weeks);
        for w in 0..weeks {
            let mut col = [0u8; 7];
            for (d, cell) in col.iter_mut().enumerate() {
                let idx = first_sunday + 7 * w as i64 + d as i64;
                if idx <= today {
                    *cell =
                        crate::stats_cache::level_for(self.by_day.get(&idx).copied().unwrap_or(0));
                }
            }
            grid.push(col);
        }
        grid
    }
}

/// Tracks how far each jsonl file has been consumed, so updates are incremental.
pub struct Cursor {
    /// path -> last fully-consumed byte offset.
    offsets: HashMap<PathBuf, u64>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

impl Cursor {
    pub fn new() -> Self {
        Cursor {
            offsets: HashMap::new(),
        }
    }

    /// Read newly appended lines across `projects_root/*/*.jsonl` (excluding any
    /// path containing `subagents`) and ingest them. Re-reads from 0 if a file
    /// shrank (rotation/truncation). Only complete (newline-terminated) lines
    /// are consumed; a half-written trailing line waits for the next call.
    /// Returns the number of new records ingested.
    pub fn update(
        &mut self,
        projects_root: &Path,
        ledger: &mut TokenLedger,
        now: SystemTime,
    ) -> Result<usize> {
        let mut new_count = 0usize;
        let mut first_error: Option<anyhow::Error> = None;

        for file in jsonl_files(projects_root) {
            let project = file
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            let len = match std::fs::metadata(&file) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            let prev = self.offsets.get(&file).copied().unwrap_or(0);
            let start = if len < prev { 0 } else { prev };

            if len == start {
                self.offsets.insert(file.clone(), len);
                continue;
            }

            let mut f = match std::fs::File::open(&file) {
                Ok(f) => f,
                Err(_) => continue,
            };
            if f.seek(SeekFrom::Start(start)).is_err() {
                continue;
            }
            let mut buf = Vec::new();
            if f.take(len - start).read_to_end(&mut buf).is_err() {
                continue;
            }

            // Consume only up to the last newline; leave any partial tail.
            let (process, consumed) = match buf.iter().rposition(|&b| b == b'\n') {
                Some(idx) => (&buf[..idx], start + idx as u64 + 1),
                None => (&buf[0..0], start),
            };
            for line_bytes in process.split(|&b| b == b'\n') {
                let line = String::from_utf8_lossy(line_bytes);
                match parse_line(&line, &project) {
                    Ok(Some(rec)) => {
                        ledger.ingest(&rec, now);
                        new_count += 1;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        if first_error.is_none() {
                            first_error = Some(err.context(format!("parsing {}", file.display())));
                        }
                    }
                }
            }
            self.offsets.insert(file.clone(), consumed);
        }

        match first_error {
            Some(err) => Err(err),
            None => Ok(new_count),
        }
    }
}

/// Two-level walk: `root/<slug>/*.jsonl`, excluding any path containing `subagents`.
fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(slugs) = std::fs::read_dir(root) else {
        return out;
    };
    for slug in slugs.flatten() {
        let dir = slug.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let fp = f.path();
            if fp.extension().and_then(|e| e.to_str()) == Some("jsonl")
                && !fp.to_string_lossy().contains("subagents")
            {
                out.push(fp);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn line(ts: &str, msg_id: &str, req: &str, model: &str, out: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","requestId":"{req}","message":{{"id":"{msg_id}","model":"{model}","usage":{{"output_tokens":{out}}}}}}}"#
        )
    }

    fn rec(ts: SystemTime, msg: &str, req: &str, model: &str, out: u64) -> AssistantRecord {
        AssistantRecord {
            message_id: msg.into(),
            request_id: req.into(),
            model: model.into(),
            output_tokens: out,
            timestamp: ts,
            project: "p".into(),
        }
    }

    // ---- parse_line ----

    #[test]
    fn parse_assistant_line() {
        let l = line("2026-06-08T05:15:25Z", "m1", "r1", "claude-opus-4-8", 1504);
        let r = parse_line(&l, "projA").unwrap().unwrap();
        assert_eq!(r.message_id, "m1");
        assert_eq!(r.request_id, "r1");
        assert_eq!(r.model, "claude-opus-4-8");
        assert_eq!(r.output_tokens, 1504);
        assert_eq!(r.project, "projA");
    }

    #[test]
    fn parse_skips_non_assistant_and_blank() {
        assert!(parse_line("", "p").unwrap().is_none());
        assert!(
            parse_line(r#"{"type":"user","message":{}}"#, "p")
                .unwrap()
                .is_none()
        );
        // assistant but no usage
        assert!(
            parse_line(r#"{"type":"assistant","message":{"id":"x"}}"#, "p")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parse_errors_on_malformed_json() {
        assert!(parse_line("{not json", "p").is_err());
    }

    // ---- TokenLedger ----

    #[test]
    fn ledger_dedups_streaming_rows() {
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let ts = iso8601_to_systemtime("2026-06-08T11:00:00Z").unwrap();
        let mut led = TokenLedger::new();
        led.ingest(&rec(ts, "m1", "r1", "opus", 100), now);
        led.ingest(&rec(ts, "m1", "r1", "opus", 100), now); // same (msg,req) => skip
        assert_eq!(led.stats(now).today_total_output, 100);
    }

    #[test]
    fn ledger_ignores_other_days() {
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let yest = iso8601_to_systemtime("2026-06-07T11:00:00Z").unwrap();
        let mut led = TokenLedger::new();
        led.ingest(&rec(yest, "m1", "r1", "opus", 100), now);
        assert_eq!(led.stats(now).today_total_output, 0);
    }

    #[test]
    fn ledger_aggregates_by_model_sorted_desc() {
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let ts = iso8601_to_systemtime("2026-06-08T11:00:00Z").unwrap();
        let mut led = TokenLedger::new();
        led.ingest(&rec(ts, "m1", "r1", "opus", 100), now);
        led.ingest(&rec(ts, "m2", "r2", "sonnet", 300), now);
        led.ingest(&rec(ts, "m3", "r3", "opus", 50), now);
        let s = led.stats(now);
        assert_eq!(s.today_total_output, 450);
        assert_eq!(
            s.by_model,
            vec![("sonnet".into(), 300), ("opus".into(), 150)]
        );
    }

    #[test]
    fn ledger_day_roll_zeros_today() {
        let now1 = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let ts = iso8601_to_systemtime("2026-06-08T11:00:00Z").unwrap();
        let mut led = TokenLedger::new();
        led.ingest(&rec(ts, "m1", "r1", "opus", 100), now1);
        assert_eq!(led.stats(now1).today_total_output, 100);
        let now2 = iso8601_to_systemtime("2026-06-09T12:00:00Z").unwrap();
        assert_eq!(led.stats(now2).today_total_output, 0);
    }

    #[test]
    fn ledger_live_rate() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let ts = now - Duration::from_secs(30);
        let mut led = TokenLedger::new();
        led.ingest(&rec(ts, "m1", "r1", "opus", 1500), now);
        // 1500 output over a 90s window => 1500 / 1.5 = 1000 tok/min
        assert_eq!(led.stats(now).live_tok_per_min, Some(1000.0));
    }

    #[test]
    fn ledger_activity_heatmap_trims_leading_empty() {
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap(); // a Monday
        let mut led = TokenLedger::new();
        led.ingest(&rec(now, "m1", "r1", "opus", 60_000), now); // today => level 2
        let two_weeks_ago = now - Duration::from_secs(14 * 86_400); // a Monday
        led.ingest(&rec(two_weeks_ago, "m2", "r2", "opus", 10_000), now); // => level 1

        // max 8 weeks, but data spans only 3 calendar weeks => trimmed to 3.
        let grid = led.activity_heatmap(now, 8);
        assert_eq!(grid.len(), 3);
        assert_eq!(grid[2][1], 2); // today (Monday), last column
        assert_eq!(grid[0][1], 1); // two-weeks-ago Monday, first column
        assert_eq!(grid[1][1], 0); // the middle week had no activity
        assert_eq!(grid[2][6], 0); // future (Saturday) stays 0
    }

    // ---- Cursor ----

    fn write_session(root: &Path, slug: &str, file: &str, contents: &str) -> PathBuf {
        let dir = root.join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(file);
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn cursor_incremental_only_reads_appended() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let l1 = line("2026-06-08T11:00:00Z", "m1", "r1", "opus", 100);
        let l2 = line("2026-06-08T11:30:00Z", "m2", "r2", "opus", 50);
        let p = write_session(&root, "projA", "sess1.jsonl", &format!("{l1}\n{l2}\n"));

        let mut cur = Cursor::new();
        let mut led = TokenLedger::new();
        assert_eq!(cur.update(&root, &mut led, now).unwrap(), 2);
        assert_eq!(led.stats(now).today_total_output, 150);

        // append a third line; only it is read next time
        let l3 = line("2026-06-08T11:45:00Z", "m3", "r3", "sonnet", 25);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{l3}").unwrap();
        assert_eq!(cur.update(&root, &mut led, now).unwrap(), 1);
        assert_eq!(led.stats(now).today_total_output, 175);
    }

    #[test]
    fn cursor_excludes_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        write_session(
            &root,
            "subagents",
            "s.jsonl",
            &format!("{}\n", line("2026-06-08T11:00:00Z", "m", "r", "opus", 100)),
        );
        let mut cur = Cursor::new();
        let mut led = TokenLedger::new();
        assert_eq!(cur.update(&root, &mut led, now).unwrap(), 0);
    }

    #[test]
    fn cursor_rereads_after_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let two = format!(
            "{}\n{}\n",
            line("2026-06-08T11:00:00Z", "m1", "r1", "opus", 100),
            line("2026-06-08T11:10:00Z", "m2", "r2", "opus", 100)
        );
        let p = write_session(&root, "projA", "sess1.jsonl", &two);
        let mut cur = Cursor::new();
        let mut led = TokenLedger::new();
        assert_eq!(cur.update(&root, &mut led, now).unwrap(), 2);

        // rotate: a shorter file (len < prev) forces a re-read from 0
        std::fs::write(
            &p,
            format!("{}\n", line("2026-06-08T11:20:00Z", "m9", "r9", "opus", 7)),
        )
        .unwrap();
        assert_eq!(cur.update(&root, &mut led, now).unwrap(), 1);
    }

    #[test]
    fn cursor_waits_for_complete_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let l1 = line("2026-06-08T11:00:00Z", "m1", "r1", "opus", 100);
        let p = write_session(&root, "projA", "sess1.jsonl", &l1); // NO trailing newline
        let mut cur = Cursor::new();
        let mut led = TokenLedger::new();
        assert_eq!(cur.update(&root, &mut led, now).unwrap(), 0); // incomplete line: not consumed

        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "\n").unwrap(); // complete the line
        assert_eq!(cur.update(&root, &mut led, now).unwrap(), 1);
    }

    #[test]
    fn cursor_reports_malformed_lines_but_keeps_good_records() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let now = iso8601_to_systemtime("2026-06-08T12:00:00Z").unwrap();
        let good = line("2026-06-08T11:00:00Z", "m1", "r1", "opus", 100);
        write_session(
            &root,
            "projA",
            "sess1.jsonl",
            &format!("{good}\n{{bad json\n"),
        );

        let mut cur = Cursor::new();
        let mut led = TokenLedger::new();
        let err = cur.update(&root, &mut led, now).unwrap_err();

        assert!(err.to_string().contains("parsing"));
        assert_eq!(led.stats(now).today_total_output, 100);
    }
}
