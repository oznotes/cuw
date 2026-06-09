// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! gpui shell: a borderless, see-through-glass, always-on-top window that renders
//! the latest `UsageSnapshot`. Written against the real gpui / gpui-component
//! API (examples/hello_world + system_monitor + menu_story in the checkout).
//!
//! Threading: the `Collector` runs on its own OS thread (blocking file/network
//! IO stays off the UI thread) and publishes snapshots into a shared slot; the
//! view repaints ~1 Hz so countdowns tick and new snapshots appear promptly.
//!
//! Interaction: drag by the header (Windows native HTCAPTION), right-click the
//! body for a menu (Refresh / Quit), Alt+F4 to quit. Position is persisted.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use gpui::*;
use gpui_component::menu::ContextMenuExt;
use gpui_component::progress::{Progress, ProgressCircle};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Root, Sizable as _, Theme, ThemeMode, h_flex, v_flex};

use usage_core::collector::Collector;
use usage_core::config::{Config, ViewMode};
use usage_core::model::{
    HeatCell, Level, LiveSource, Provenance, UsageSnapshot, Window as UWindow,
};

use crate::win;

mod theme;

actions!(claude_usage_widget, [Quit, RefreshNow, ToggleAutostart]);

type Shared = Arc<Mutex<Option<UsageSnapshot>>>;

/// Launch the widget. Blocks until the application quits.
pub fn run(config: Config) -> Result<()> {
    let shared: Shared = Arc::new(Mutex::new(None));
    let refresh: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // Collector on a dedicated OS thread — blocking IO off the UI thread.
    // Ticks every `refresh_secs`, or immediately when the refresh flag is set.
    {
        let shared = shared.clone();
        let cfg = config.clone();
        let refresh = refresh.clone();
        std::thread::Builder::new()
            .name("usage-collector".into())
            .spawn(move || {
                let mut collector = Collector::new(cfg.clone());
                let period = Duration::from_secs(cfg.refresh_secs.max(1));
                let mut last: Option<Instant> = None;
                loop {
                    let due = last.map_or(true, |t| t.elapsed() >= period);
                    if due || refresh.swap(false, Ordering::SeqCst) {
                        let snap = collector.tick(SystemTime::now());
                        if let Ok(mut g) = shared.lock() {
                            *g = Some(snap);
                        }
                        last = Some(Instant::now());
                    }
                    std::thread::sleep(Duration::from_millis(400));
                }
            })
            .ok();
    }

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);

        // Actions: Quit closes the app; RefreshNow pokes the collector thread.
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.on_action({
            let refresh = refresh.clone();
            move |_: &RefreshNow, _cx: &mut App| refresh.store(true, Ordering::SeqCst)
        });
        // Stateless registry flip, handled at the App level: a context-menu's
        // action dispatch reaches App handlers but NOT view-level ones.
        cx.on_action(|_: &ToggleAutostart, _cx: &mut App| {
            let _ = win::autostart::set(!win::autostart::is_enabled());
        });
        cx.bind_keys([KeyBinding::new("alt-f4", Quit, None)]);

        let shared = shared.clone();
        let config = config.clone();
        cx.spawn(async move |cx| {
            let opts = window_options(&config);
            let opened = cx.open_window(opts, |window, cx| {
                win::topmost::pin(window);
                Theme::change(ThemeMode::Dark, Some(window), cx);
                let view = cx.new(|cx| Widget::new(shared.clone(), config.clone(), cx));
                // gpui-component's `Root` otherwise paints the whole window with
                // `cx.theme().background` (root.rs). Override only this root so
                // menus/tooltips keep normal theme surfaces.
                cx.new(|cx| Root::new(view, window, cx).bg(gpui::hsla(0.0, 0.0, 0.0, 0.0)))
            });
            if let Err(e) = opened {
                // With panic="abort" + windows_subsystem="windows", a panic here
                // would make the process vanish silently. Fail loudly to stderr
                // (harmless in release) and shut the app down cleanly instead.
                eprintln!("claude-usage-widget: failed to open window: {e}");
                let _ = cx.update(|cx| cx.quit());
            }
        })
        .detach();
    });

    Ok(())
}

fn window_options(config: &Config) -> WindowOptions {
    let origin = config
        .position
        .map(|(x, y)| point(px(x), px(y)))
        .unwrap_or_else(|| point(px(48.0), px(48.0)));
    WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: widget_size(false),
        })),
        // Transparent window so the desktop shows through (real see-through
        // glass). `opacity` (the panel scrim) controls how much.
        window_background: gpui::WindowBackgroundAppearance::Transparent,
        is_resizable: false,
        is_minimizable: false,
        kind: WindowKind::Normal,
        ..Default::default()
    }
}

fn widget_size(show_details: bool) -> Size<Pixels> {
    size(px(300.0), px(if show_details { 392.0 } else { 196.0 }))
}

struct Widget {
    shared: Shared,
    config: Config,
    last_saved_pos: Option<(f32, f32)>,
    pending_pos: Option<(f32, f32)>,
    pos_changed_at: Option<std::time::Instant>,
    show_details: bool,
}

impl Widget {
    fn new(shared: Shared, config: Config, cx: &mut Context<Self>) -> Self {
        // Seed the last-saved position from config so the very first paint (which
        // reports the window's current origin) doesn't trigger a spurious write.
        let seed_pos = config.position;

        // Repaint ~1 Hz so reset countdowns advance and fresh snapshots show.
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                if this.update(cx, |_this, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();

        Widget {
            shared,
            config,
            last_saved_pos: seed_pos,
            pending_pos: None,
            pos_changed_at: None,
            show_details: false,
        }
    }

    fn level(&self, used_pct: f32) -> Level {
        Level::from_pct(
            used_pct,
            self.config.warn_threshold,
            self.config.critical_threshold,
        )
    }

    /// Persist the window position when it changes (after a drag settles).
    fn save_position_if_moved(&mut self, window: &Window) {
        let o = window.bounds().origin;
        let pos = (f32::from(o.x), f32::from(o.y));
        let same =
            |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() <= 1.0 && (a.1 - b.1).abs() <= 1.0;
        if self.last_saved_pos.map_or(false, |s| same(s, pos)) {
            return; // already saved this position
        }
        // Debounce: wait until the position has stopped changing for ~800ms,
        // so a drag doesn't write to disk on every frame.
        if self.pending_pos.map_or(true, |p| !same(p, pos)) {
            self.pending_pos = Some(pos);
            self.pos_changed_at = Some(std::time::Instant::now());
            return;
        }
        if self.pos_changed_at.map_or(false, |t| {
            t.elapsed() >= std::time::Duration::from_millis(800)
        }) {
            self.last_saved_pos = Some(pos);
            self.config.position = Some(pos);
            let _ = self.config.save();
            self.pending_pos = None;
            self.pos_changed_at = None;
        }
    }

    fn window_row(&self, label: &str, w: &UWindow, now: SystemTime) -> impl IntoElement {
        let color = theme::level_color(self.level(w.used_pct));
        let id: SharedString = format!("bar-{label}").into();
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(22.)).text_sm().child(label.to_string()))
                    .child(
                        div().flex_1().child(
                            Progress::new(id)
                                .h_2()
                                .value(w.used_pct.clamp(0., 100.))
                                .color(color),
                        ),
                    )
                    .child(
                        div()
                            .w(px(42.))
                            .text_sm()
                            .text_color(color)
                            .child(format!("{:.0}%", w.used_pct)),
                    ),
            )
            .child(div().text_xs().child(fmt_reset(w.resets_at, now)))
    }

    fn flip_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_details = !self.show_details;
        window.resize(widget_size(self.show_details));
        cx.notify();
    }

    /// A small centered chevron at the bottom that expands/collapses Details.
    fn expand_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let glyph = if self.show_details { "▴" } else { "▾" };
        let muted = cx.theme().muted_foreground;
        h_flex()
            .id("expand-toggle")
            .w_full()
            .justify_center()
            .child(div().text_xs().text_color(muted).child(glyph))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| this.flip_details(window, cx)),
            )
    }

    fn cycle_view(&mut self, cx: &mut Context<Self>) {
        self.config.view_mode = match self.config.view_mode {
            ViewMode::Bars => ViewMode::Gauge,
            ViewMode::Gauge => ViewMode::Bars,
        };
        let _ = self.config.save();
        cx.notify();
    }

    fn cycle_opacity(&mut self, cx: &mut Context<Self>) {
        // 0.0 = fully see-through, up to 0.85 = mostly solid card.
        const STEPS: [f32; 5] = [0.0, 0.15, 0.3, 0.55, 0.85];
        let i = STEPS
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (*a - self.config.opacity)
                    .abs()
                    .partial_cmp(&(*b - self.config.opacity).abs())
                    .unwrap()
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.config.opacity = STEPS[(i + 1) % STEPS.len()];
        let _ = self.config.save();
        cx.notify();
    }

    /// The gauge (rings) variant of the 5H/7D display.
    fn gauge_view(&self, snap: &UsageSnapshot, now: SystemTime, muted: Hsla) -> impl IntoElement {
        h_flex()
            .w_full()
            .justify_around()
            .items_start()
            .py_1()
            .child(self.ring("5H", &snap.five_hour, now, muted))
            .child(self.ring("7D", &snap.seven_day, now, muted))
    }

    fn ring(&self, label: &str, w: &UWindow, now: SystemTime, muted: Hsla) -> impl IntoElement {
        let color = theme::level_color(self.level(w.used_pct));
        let id: SharedString = format!("ring-{label}").into();
        v_flex()
            .items_center()
            .gap_1()
            .child(
                ProgressCircle::new(id)
                    .value(w.used_pct.clamp(0., 100.))
                    .color(color)
                    .with_size(gpui_component::Size::Size(px(64.))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color)
                    .child(format!("{label} {:.0}%", w.used_pct)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(fmt_reset(w.resets_at, now)),
            )
    }

    /// A small row of clickable settings in Details: view / opacity.
    fn settings_row(&self, muted: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let view_v = match self.config.view_mode {
            ViewMode::Bars => "bars",
            ViewMode::Gauge => "rings",
        };
        let opacity_v = format!("{:.0}%", self.config.opacity * 100.);
        h_flex()
            .gap_3()
            .pt_1()
            .text_xs()
            .text_color(muted)
            .child(
                div()
                    .id("set-view")
                    .child(format!("view {view_v}"))
                    .tooltip(|window, cx| {
                        Tooltip::new("click to toggle bars/rings").build(window, cx)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _w, cx| this.cycle_view(cx)),
                    ),
            )
            .child(
                div()
                    .id("set-opacity")
                    .child(format!("opacity {opacity_v}"))
                    .tooltip(|window, cx| Tooltip::new("click to cycle opacity").build(window, cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _w, cx| this.cycle_opacity(cx)),
                    ),
            )
    }
}

impl Render for Widget {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.save_position_if_moved(window);

        let snap = self.shared.lock().ok().and_then(|g| g.clone());
        let now = SystemTime::now();
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;

        // The widget window is transparent (see-through to the desktop); this
        // dark scrim is the only fill. `opacity` blends 0.0 (fully see-through)
        // to 1.0 (solid dark card). Text/bars stay opaque on top.
        let root = v_flex()
            .id("widget-root")
            .size_full()
            .gap_2()
            .p_3()
            .rounded_xl()
            .bg(theme::panel_bg(self.config.opacity))
            .text_color(fg);

        let root = match snap {
            None => root.child(div().text_sm().text_color(muted).child("Loading usage…")),
            Some(snap) => {
                let prov = source_label(&snap, now);
                let footer = footer_text(&snap);
                let diagnostics = diagnostic_text(&snap, now);
                // Header is the drag handle (native HTCAPTION on Windows).
                let header = h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().child("Claude · Max 5×"))
                    .child(div().text_xs().text_color(muted).child(prov))
                    .window_control_area(WindowControlArea::Drag);

                let mut root = root.child(header);
                root = match self.config.view_mode {
                    ViewMode::Bars => root
                        .child(self.window_row("5H", &snap.five_hour, now))
                        .child(self.window_row("7D", &snap.seven_day, now)),
                    ViewMode::Gauge => root.child(self.gauge_view(&snap, now, muted)),
                };
                root = root.child(div().text_xs().text_color(muted).child(footer));
                if self.show_details {
                    root = root.child(details_panel(&snap, now, muted));
                    root = root.child(self.settings_row(muted, cx));
                    if let Some(diagnostics) = diagnostics {
                        root = root.child(div().text_xs().text_color(muted).child(diagnostics));
                    }
                }
                root.child(self.expand_toggle(cx))
            }
        };

        // Right-click anywhere on the body for the menu (drag header excepted,
        // where Windows shows its own system menu).
        // Read the live registry state and use a state-reflecting label, so it's
        // unambiguous what clicking does (the menu_with_check tick is too subtle).
        let autostart_label = if win::autostart::is_enabled() {
            "Disable start-on-login"
        } else {
            "Enable start-on-login"
        };
        root.context_menu(move |menu, _window, _cx| {
            menu.menu("Refresh now", Box::new(RefreshNow))
                .menu(autostart_label, Box::new(ToggleAutostart))
                .separator()
                .menu("Quit", Box::new(Quit))
        })
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn footer_text(snap: &UsageSnapshot) -> String {
    let mut parts = vec![format!(
        "today {}",
        fmt_tokens(snap.tokens.today_total_output)
    )];
    if let Some(rate) = snap.tokens.live_tok_per_min {
        parts.push(format!("live {}/m", fmt_tokens(rate.round() as u64)));
    }
    if let Some(opus) = &snap.seven_day_opus {
        parts.push(format!("opus {:.0}%", opus.used_pct));
    }
    parts.join(" · ")
}

fn details_panel(snap: &UsageSnapshot, now: SystemTime, muted: Hsla) -> impl IntoElement {
    let mut panel = v_flex()
        .gap_1()
        .pt_1()
        .child(div().h(px(1.0)).bg(rgba(0x56607055)))
        .child(projects_section(&snap.tokens.top_projects, muted));

    if has_activity(&snap.tokens.activity_heatmap) {
        panel = panel.child(activity_grid(&snap.tokens.activity_heatmap, muted));
    }

    if let Some(opus) = &snap.seven_day_opus {
        panel = panel.child(detail_text_row(
            "opus",
            format!("{:.0}% · {}", opus.used_pct, fmt_reset(opus.resets_at, now)),
            muted,
        ));
    }

    panel
}

fn detail_text_row(label: &'static str, value: String, muted: Hsla) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap_2()
        .child(div().w(px(48.0)).text_xs().text_color(muted).child(label))
        .child(div().flex_1().text_xs().child(value))
}

fn has_activity(grid: &[[HeatCell; 7]]) -> bool {
    grid.iter().any(|week| week.iter().any(|c| c.level > 0))
}

/// GitHub-contributions-style grid: weeks are columns, Sun..Sat are rows.
/// Each cell shows its date + token count on hover.
fn activity_grid(grid: &[[HeatCell; 7]], muted: Hsla) -> impl IntoElement {
    let mut cols = h_flex().items_start().gap_0p5();
    for (wi, week) in grid.iter().enumerate() {
        let mut col = v_flex().gap_0p5();
        for (di, cell) in week.iter().enumerate() {
            let mut dot = div()
                .id(("heatcell", wi * 7 + di))
                .w(px(9.0))
                .h(px(9.0))
                .rounded_sm()
                .bg(theme::heatmap_color(cell.level));
            if !cell.label.is_empty() {
                let tip = if cell.tokens > 0 {
                    format!("{} · {}", cell.label, fmt_tokens(cell.tokens))
                } else {
                    format!("{} · no activity", cell.label)
                };
                dot = dot.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
            }
            col = col.child(dot);
        }
        cols = cols.child(col);
    }

    h_flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(48.0))
                .text_xs()
                .text_color(muted)
                .child("activity"),
        )
        .child(cols)
}

/// Top projects today as a small aligned list: `name … tokens`, one per line.
fn projects_section(projects: &[(String, u64)], muted: Hsla) -> impl IntoElement {
    let mut list = v_flex().flex_1().gap_0p5();
    if projects.is_empty() {
        list = list.child(div().text_xs().text_color(muted).child("no activity today"));
    } else {
        for (slug, tokens) in projects.iter().take(3) {
            list = list.child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .truncate()
                            .child(project_label(slug)),
                    )
                    .child(div().text_xs().text_color(muted).child(fmt_tokens(*tokens))),
            );
        }
    }

    h_flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .w(px(48.0))
                .text_xs()
                .text_color(muted)
                .child("projects"),
        )
        .child(list)
}

/// Turn a Claude project slug (the cwd with separators replaced by `-`, e.g.
/// `C--Users-oz-Desktop-claude-usage`) into something readable by stripping the
/// home-directory prefix, leaving the path under home (e.g. `Desktop-claude-usage`).
fn project_label(slug: &str) -> String {
    let home = home_slug();
    let rest = (!home.is_empty())
        .then(|| slug.strip_prefix(&home))
        .flatten()
        .map(|r| r.trim_start_matches('-'))
        .filter(|r| !r.is_empty())
        .unwrap_or(slug);
    short_tail(rest, 24)
}

/// Build the slug prefix for the current home directory, matching Claude's encoding.
fn home_slug() -> String {
    std::env::var("USERPROFILE")
        .unwrap_or_default()
        .replace([':', '\\', '/'], "-")
}

/// Truncate keeping the END (the deepest, most identifying path component).
fn short_tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n + 1 - max).collect();
    format!("…{tail}")
}

fn source_label(snap: &UsageSnapshot, now: SystemTime) -> String {
    match &snap.source {
        Provenance::StatusLine => snap
            .diagnostics
            .statusline_age_secs
            .map(|age| format!("live {}", fmt_duration(age)))
            .unwrap_or_else(|| "live".into()),
        Provenance::OAuth => "oauth".into(),
        Provenance::Stale { last_good_at } => {
            if snap.diagnostics.last_quota_success.is_some() {
                format!("stale {}", fmt_age(*last_good_at, now))
            } else {
                "stale".into()
            }
        }
    }
}

fn diagnostic_text(snap: &UsageSnapshot, now: SystemTime) -> Option<String> {
    let mut parts = Vec::new();
    if matches!(snap.source, Provenance::Stale { .. }) {
        if let Some(source) = snap.diagnostics.last_quota_success {
            parts.push(format!("last {}", live_source_label(source)));
        }
        if let Some(age) = snap.diagnostics.statusline_age_secs {
            parts.push(format!("cache {}", fmt_duration(age)));
        }
    }
    if snap.diagnostics.oauth_error.is_some() {
        parts.push("oauth issue".into());
    }
    if snap.diagnostics.jsonl_error.is_some() {
        parts.push("jsonl issue".into());
    }
    if let Provenance::Stale { last_good_at } = snap.source
        && snap.diagnostics.last_quota_success.is_some()
    {
        parts.push(format!("reading {}", fmt_age(last_good_at, now)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn live_source_label(source: LiveSource) -> &'static str {
    match source {
        LiveSource::StatusLine => "live",
        LiveSource::OAuth => "oauth",
    }
}

fn fmt_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn fmt_age(t: SystemTime, now: SystemTime) -> String {
    now.duration_since(t)
        .map(|d| format!("{} ago", fmt_duration(d.as_secs())))
        .unwrap_or_else(|_| "now".into())
}

fn fmt_reset(resets_at: Option<SystemTime>, now: SystemTime) -> String {
    let Some(t) = resets_at else {
        return String::new();
    };
    let Ok(d) = t.duration_since(now) else {
        return "resets now".into();
    };
    let secs = d.as_secs();
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h >= 24 {
        format!("resets in {}d {}h", h / 24, h % 24)
    } else if h > 0 {
        format!("resets in {h}h {m}m")
    } else {
        format!("resets in {m}m")
    }
}
