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
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeMode, h_flex, progress::Progress, v_flex};

use usage_core::collector::Collector;
use usage_core::config::Config;
use usage_core::model::{Level, Provenance, TokenStats, UsageSnapshot, Window as UWindow};

use crate::win;

mod theme;

actions!(claude_usage_widget, [Quit, RefreshNow]);

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
    last_saved_pos: Option<(f32, f32)>,
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

        Widget { shared, config, last_saved_pos: None }
    }

    fn level(&self, used_pct: f32) -> Level {
        Level::from_pct(used_pct, self.config.warn_threshold, self.config.critical_threshold)
    }

    /// Persist the window position when it changes (after a drag settles).
    fn save_position_if_moved(&mut self, window: &Window) {
        let o = window.bounds().origin;
        let pos = (f32::from(o.x), f32::from(o.y));
        let moved = self
            .last_saved_pos
            .map_or(true, |p| (p.0 - pos.0).abs() > 1.0 || (p.1 - pos.1).abs() > 1.0);
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
            .bg(rgba(0x1a1d26cc))
            .text_color(fg);

        let root = match snap {
            None => root.child(div().text_sm().text_color(muted).child("Loading usage…")),
            Some(snap) => {
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
                // Header is the drag handle (native HTCAPTION on Windows).
                let header = h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().child("Claude · Max 5×"))
                    .child(div().text_xs().text_color(muted).child(prov))
                    .window_control_area(WindowControlArea::Drag);

                root.child(header)
                    .child(self.window_row("5H", &snap.five_hour, now))
                    .child(self.window_row("7D", &snap.seven_day, now))
                    .child(div().text_xs().text_color(muted).child(footer))
            }
        };

        // Right-click anywhere on the body for the menu (drag header excepted,
        // where Windows shows its own system menu).
        root.context_menu(|menu, _window, _cx| {
            menu.menu("Refresh now", Box::new(RefreshNow))
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
