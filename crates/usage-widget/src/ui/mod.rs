//! gpui shell: a borderless, frosted-glass, always-on-top window that renders
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
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme as _, Root, Theme, ThemeMode, h_flex, progress::Progress, v_flex,
};

use usage_core::collector::Collector;
use usage_core::config::Config;
use usage_core::model::{
    HeatCell, Level, LiveSource, Provenance, UsageSnapshot, Window as UWindow,
};

use crate::win;

mod theme;

actions!(
    claude_usage_widget,
    [Quit, RefreshNow, ToggleDetails, ToggleAutostart]
);

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
        cx.bind_keys([KeyBinding::new("alt-f4", Quit, None)]);

        let shared = shared.clone();
        let config = config.clone();
        cx.spawn(async move |cx| {
            let opts = window_options(&config);
            cx.open_window(opts, |window, cx| {
                win::topmost::pin(window);
                Theme::change(ThemeMode::Dark, Some(window), cx);
                let view = cx.new(|cx| Widget::new(shared.clone(), config.clone(), cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open widget window");
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
        window_background: win::backdrop::appearance(config.backdrop),
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
    show_details: bool,
    autostart_enabled: bool,
}

impl Widget {
    fn new(shared: Shared, config: Config, cx: &mut Context<Self>) -> Self {
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
            last_saved_pos: None,
            show_details: false,
            autostart_enabled: win::autostart::is_enabled(),
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
        let moved = self.last_saved_pos.map_or(true, |p| {
            (p.0 - pos.0).abs() > 1.0 || (p.1 - pos.1).abs() > 1.0
        });
        if moved {
            self.last_saved_pos = Some(pos);
            self.config.position = Some(pos);
            let _ = self.config.save();
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

    fn toggle_details(&mut self, _: &ToggleDetails, window: &mut Window, cx: &mut Context<Self>) {
        self.flip_details(window, cx);
    }

    fn toggle_autostart(
        &mut self,
        _: &ToggleAutostart,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.autostart_enabled = !self.autostart_enabled;
        if win::autostart::set(self.autostart_enabled).is_err() {
            self.autostart_enabled = !self.autostart_enabled; // revert if it failed
        }
        cx.notify();
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
}

impl Render for Widget {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.save_position_if_moved(window);

        let snap = self.shared.lock().ok().and_then(|g| g.clone());
        let now = SystemTime::now();
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;

        // Dark, semi-transparent panel: legible over the Mica frosted backdrop.
        let root = v_flex()
            .id("widget-root")
            .size_full()
            .gap_2()
            .p_3()
            .rounded_xl()
            .bg(theme::scrim())
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

                let mut root = root
                    .child(header)
                    .child(self.window_row("5H", &snap.five_hour, now))
                    .child(self.window_row("7D", &snap.seven_day, now))
                    .child(div().text_xs().text_color(muted).child(footer));
                if self.show_details {
                    root = root.child(details_panel(&snap, now, muted));
                    if let Some(diagnostics) = diagnostics {
                        root = root.child(div().text_xs().text_color(muted).child(diagnostics));
                    }
                }
                root.child(self.expand_toggle(cx))
            }
        };

        // Right-click anywhere on the body for the menu (drag header excepted,
        // where Windows shows its own system menu).
        let show_details = self.show_details;
        let autostart_on = self.autostart_enabled;
        root.on_action(cx.listener(Self::toggle_details))
            .on_action(cx.listener(Self::toggle_autostart))
            .context_menu(move |menu, _window, _cx| {
                menu.menu("Refresh now", Box::new(RefreshNow))
                    .menu_with_check("Details", show_details, Box::new(ToggleDetails))
                    .menu_with_check("Start on login", autostart_on, Box::new(ToggleAutostart))
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

/// `C:\Users\oz` → `C--Users-oz`, matching Claude's slug encoding.
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
