//! gpui shell: a borderless, frosted-glass, always-on-top window that renders
//! the latest `UsageSnapshot`. Written against the real gpui / gpui-component
//! API (see examples/hello_world + system_monitor in the pinned checkout).
//!
//! Threading: the `Collector` runs on its own OS thread (blocking file/network
//! IO stays off the UI thread) and publishes snapshots into a shared slot; the
//! view repaints ~1 Hz so countdowns tick and new snapshots appear promptly.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use gpui::*;
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeMode, h_flex, progress::Progress, v_flex};

use usage_core::collector::Collector;
use usage_core::config::Config;
use usage_core::model::{Level, Provenance, TokenStats, UsageSnapshot, Window as UWindow};

use crate::win;

mod theme;

type Shared = Arc<Mutex<Option<UsageSnapshot>>>;

/// Launch the widget. Blocks until the application quits.
pub fn run(config: Config) -> Result<()> {
    let shared: Shared = Arc::new(Mutex::new(None));

    // Collector on a dedicated OS thread — blocking IO off the UI thread.
    {
        let shared = shared.clone();
        let cfg = config.clone();
        std::thread::Builder::new()
            .name("usage-collector".into())
            .spawn(move || {
                let mut collector = Collector::new(cfg.clone());
                let period = Duration::from_secs(cfg.refresh_secs.max(1));
                loop {
                    let snap = collector.tick(SystemTime::now());
                    if let Ok(mut g) = shared.lock() {
                        *g = Some(snap);
                    }
                    std::thread::sleep(period);
                }
            })
            .ok();
    }

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let shared = shared.clone();
        let config = config.clone();
        cx.spawn(async move |cx| {
            let opts = window_options(&config);
            cx.open_window(opts, |window, cx| {
                // gpui has no always-on-top on Windows; pin it ourselves.
                win::topmost::pin(window);
                // Dark theme so light text sits well on the frosted-glass panel.
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
        titlebar: None, // borderless
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: size(px(300.0), px(168.0)),
        })),
        window_background: win::backdrop::appearance(config.backdrop),
        is_resizable: false,
        is_minimizable: false,
        kind: WindowKind::Normal,
        ..Default::default()
    }
}

struct Widget {
    shared: Shared,
    config: Config,
}

impl Widget {
    fn new(shared: Shared, config: Config, cx: &mut Context<Self>) -> Self {
        // Repaint ~1 Hz so reset countdowns advance and fresh snapshots show.
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                if this.update(cx, |_this, cx| cx.notify()).is_err() {
                    break; // view dropped
                }
            }
        })
        .detach();

        Widget { shared, config }
    }

    fn level(&self, used_pct: f32) -> Level {
        Level::from_pct(used_pct, self.config.warn_threshold, self.config.critical_threshold)
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
}

impl Render for Widget {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = self.shared.lock().ok().and_then(|g| g.clone());
        let now = SystemTime::now();
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;

        // Dark, semi-transparent panel: legible over the Mica frosted backdrop.
        // The whole panel is a drag handle (borderless window has no title bar).
        let root = v_flex()
            .size_full()
            .gap_2()
            .p_3()
            .rounded_xl()
            .bg(rgba(0x1a1d26cc))
            .text_color(fg)
            .on_mouse_down(MouseButton::Left, |_, window, _cx| {
                window.start_window_move();
            });

        let Some(snap) = snap else {
            return root.child(div().text_sm().text_color(muted).child("Loading usage…"));
        };

        let prov = match &snap.source {
            Provenance::StatusLine => "live",
            Provenance::OAuth => "oauth",
            Provenance::Stale { .. } => "stale",
        };
        let footer = format!(
            "today {} · {}",
            fmt_tokens(snap.tokens.today_total_output),
            top_model(&snap.tokens)
        );

        root.child(
            h_flex()
                .justify_between()
                .items_center()
                .child(div().text_sm().child("Claude · Max 5×"))
                .child(div().text_xs().text_color(muted).child(prov)),
        )
        .child(self.window_row("5H", &snap.five_hour, now))
        .child(self.window_row("7D", &snap.seven_day, now))
        .child(div().text_xs().text_color(muted).child(footer))
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

fn top_model(tokens: &TokenStats) -> String {
    tokens
        .by_model
        .first()
        .map(|(m, _)| m.clone())
        .unwrap_or_else(|| "—".into())
}

fn fmt_reset(resets_at: Option<SystemTime>, now: SystemTime) -> String {
    let Some(t) = resets_at else { return String::new() };
    let Ok(d) = t.duration_since(now) else { return "resets now".into() };
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
