// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! Widget configuration: window placement, view mode, glass opacity, color
//! thresholds, and refresh cadence.
//!
//! Persisted as JSON at `%APPDATA%/claude-usage/widget-config.json`. Loading is
//! **infallible** — a missing or corrupt file yields defaults — and unknown
//! fields are ignored, so old binaries tolerate new config keys.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    Bars,
    Gauge,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Saved top-left window position in logical pixels, if the user moved it.
    pub position: Option<(f32, f32)>,
    pub view_mode: ViewMode,
    /// See-through glass: 0.0 = fully transparent (desktop shows through), 1.0 =
    /// solid card. The widget window is transparent; this is the dark scrim over
    /// it (text stays opaque). Lower = more see-through.
    pub opacity: f32,
    /// Lower bound of the amber band.
    pub warn_threshold: f32,
    /// Lower bound of the red band.
    pub critical_threshold: f32,
    /// How often to rebuild the snapshot (seconds).
    pub refresh_secs: u64,
    /// Minimum seconds between OAuth usage polls (endpoint asks for >= 180).
    pub quota_poll_secs: u64,
    /// A status-line cache reading newer than this (seconds) is preferred over OAuth.
    pub statusline_max_age_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            position: None,
            view_mode: ViewMode::Bars,
            // See-through glass; opacity is genuine transparency (0.4 = a readable
            // balance between see-through and legible).
            opacity: 0.4,
            warn_threshold: 70.0,
            critical_threshold: 90.0,
            refresh_secs: 30,
            quota_poll_secs: 180,
            statusline_max_age_secs: 120,
        }
    }
}

impl Config {
    /// `%APPDATA%/claude-usage/widget-config.json` (falls back to the CWD if no
    /// config dir is discoverable, which should never happen on Windows).
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("claude-usage")
            .join("widget-config.json")
    }

    /// Load from the default path. Never fails.
    pub fn load() -> Config {
        Self::load_from(&Self::path())
    }

    /// Load from an explicit path; defaults on missing/unreadable/corrupt file.
    pub fn load_from(path: &Path) -> Config {
        let config = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Config::default(),
        };
        config.normalized()
    }

    /// Keep hand-edited or old config files from creating impossible UI states.
    pub fn normalized(mut self) -> Config {
        let defaults = Config::default();

        self.position = self
            .position
            .filter(|(x, y)| x.is_finite() && y.is_finite());

        self.opacity = if self.opacity.is_finite() {
            self.opacity.clamp(0.0, 1.0)
        } else {
            defaults.opacity
        };

        if !self.warn_threshold.is_finite()
            || !self.critical_threshold.is_finite()
            || self.warn_threshold >= self.critical_threshold
        {
            self.warn_threshold = defaults.warn_threshold;
            self.critical_threshold = defaults.critical_threshold;
        } else {
            self.warn_threshold = self.warn_threshold.clamp(0.0, 100.0);
            self.critical_threshold = self.critical_threshold.clamp(0.0, 100.0);
            if self.warn_threshold >= self.critical_threshold {
                self.warn_threshold = defaults.warn_threshold;
                self.critical_threshold = defaults.critical_threshold;
            }
        }

        self.refresh_secs = self.refresh_secs.max(1);
        self.quota_poll_secs = self.quota_poll_secs.max(1);
        self.statusline_max_age_secs = self.statusline_max_age_secs.max(1);

        self
    }

    /// Save to the default path.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    /// Save to an explicit path, creating parent dirs, writing atomically
    /// (temp file + rename) so a reader never sees a half-written file.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating config dir {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        // Per-process temp name so two writers don't race on the same temp file.
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, json.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        // std::fs::rename replaces an existing destination on Windows
        // (MoveFileExW + MOVEFILE_REPLACE_EXISTING).
        std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.view_mode, ViewMode::Bars);
        assert_eq!(c.opacity, 0.4);
        assert_eq!(c.warn_threshold, 70.0);
        assert_eq!(c.critical_threshold, 90.0);
        assert_eq!(c.refresh_secs, 30);
        assert_eq!(c.quota_poll_secs, 180);
        assert_eq!(c.statusline_max_age_secs, 120);
        assert_eq!(c.position, None);
    }

    #[test]
    fn json_round_trips() {
        let c = Config {
            position: Some((100.0, 200.0)),
            view_mode: ViewMode::Gauge,
            opacity: 0.55,
            ..Config::default()
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn partial_json_fills_missing_with_defaults() {
        let c: Config = serde_json::from_str(r#"{"opacity": 0.5, "view_mode": "Gauge"}"#).unwrap();
        assert_eq!(c.opacity, 0.5);
        assert_eq!(c.view_mode, ViewMode::Gauge);
        // untouched fields keep their defaults
        assert_eq!(c.warn_threshold, 70.0);
        assert_eq!(c.refresh_secs, 30);
    }

    #[test]
    fn empty_object_is_all_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // forward/back compatibility: unknown keys (incl. the removed `scale`)
        // must not break parsing.
        let c: Config =
            serde_json::from_str(r#"{"scale": 1.2, "opacity": 0.6, "future_feature": {"x": 1}}"#)
                .unwrap();
        assert_eq!(c.opacity, 0.6);
    }

    #[test]
    fn loaded_config_is_normalized() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("widget-config.json");
        std::fs::write(
            &p,
            r#"{
                "position": [10.0, 20.0],
                "opacity": 9.0,
                "warn_threshold": 95.0,
                "critical_threshold": 90.0,
                "refresh_secs": 0,
                "quota_poll_secs": 0,
                "statusline_max_age_secs": 0
            }"#,
        )
        .unwrap();

        let c = Config::load_from(&p);
        assert_eq!(c.position, Some((10.0, 20.0)));
        assert_eq!(c.opacity, 1.0);
        assert_eq!(c.warn_threshold, 70.0);
        assert_eq!(c.critical_threshold, 90.0);
        assert_eq!(c.refresh_secs, 1);
        assert_eq!(c.quota_poll_secs, 1);
        assert_eq!(c.statusline_max_age_secs, 1);
    }

    #[test]
    fn corrupt_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("widget-config.json");
        std::fs::write(&p, b"{ this is not json").unwrap();
        assert_eq!(Config::load_from(&p), Config::default());
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("does-not-exist.json");
        assert_eq!(Config::load_from(&p), Config::default());
    }

    #[test]
    fn save_then_load_is_identity() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("widget-config.json");
        let c = Config {
            opacity: 0.7,
            view_mode: ViewMode::Gauge,
            ..Config::default()
        };
        c.save_to(&p).unwrap();
        assert!(p.exists());
        assert_eq!(Config::load_from(&p), c);
    }
}
