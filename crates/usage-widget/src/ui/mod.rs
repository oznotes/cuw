//! gpui shell: a borderless, frosted-glass, always-on-top window that renders
//! the latest `UsageSnapshot`.
//!
//! ⚠️  THIS IS THE M0 STARTING SCAFFOLD, NOT A FINISHED UI.  ⚠️
//! gpui is git-only and its API is unstable. Bring this up against the pinned
//! `gpui-component` `examples/system_monitor` and `examples/hello_world`, fixing
//! the calls marked `M0:`/`M2:` as you go. The data layer it consumes
//! (`Collector` → `UsageSnapshot`) is final and fully tested.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::Result;
use usage_core::collector::Collector;
use usage_core::config::Config;
use usage_core::model::{Level, UsageSnapshot};

use crate::win;

mod theme;

/// The latest snapshot, shared between the background poller and the UI thread.
type Shared = Arc<Mutex<Option<UsageSnapshot>>>;

/// Launch the widget. Blocks until the window is closed.
pub fn run(config: Config) -> Result<()> {
    let shared: Shared = Arc::new(Mutex::new(None));

    // Seed one snapshot synchronously so the first frame isn't empty.
    {
        let mut collector = Collector::new(config.clone());
        *shared.lock().unwrap() = Some(collector.tick(SystemTime::now()));
    }

    // M0: the entry point is `gpui_platform::application()`. The root view MUST
    // be wrapped in `gpui_component::Root::new(view, window, cx)` or overlays,
    // tooltips, and theming break.
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);

        let origin = config
            .position
            .map(|(x, y)| gpui::point(gpui::px(x), gpui::px(y)))
            .unwrap_or_else(|| gpui::point(gpui::px(40.0), gpui::px(40.0)));
        let bounds = gpui::Bounds { origin, size: gpui::size(gpui::px(300.0), gpui::px(160.0)) };

        let options = gpui::WindowOptions {
            titlebar: None,
            is_resizable: false,
            is_minimizable: false,
            window_background: win::backdrop::appearance(config.backdrop),
            window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
            ..Default::default()
        };

        let window = cx
            .open_window(options, |window, cx| {
                let view = cx.new(|_cx| Widget { shared: shared.clone(), config: config.clone() });
                gpui_component::Root::new(view.into(), window, cx)
            })
            .expect("open widget window");

        // Always-on-top — the one thing gpui won't do on Windows for us.
        // M0: read the raw HWND from `window` via raw-window-handle, then:
        //     win::topmost::apply_topmost(hwnd);
        let _ = (&window, &win::topmost::apply_topmost);

        // M2: start the refresh loop. Run the collector on a background thread
        // (it is `Send`) and push results into `shared`, then request a repaint:
        //
        //   cx.spawn(async move |cx| {
        //       let mut collector = Collector::new(config.clone());
        //       loop {
        //           cx.background_executor()
        //               .timer(Duration::from_secs(config.refresh_secs))
        //               .await;
        //           let snap = collector.tick(SystemTime::now());
        //           *shared.lock().unwrap() = Some(snap);
        //           // notify the window to repaint (cx.refresh / window.refresh)
        //       }
        //   }).detach();
    });

    Ok(())
}

/// The root view. Renders whichever snapshot is currently in `shared`.
struct Widget {
    shared: Shared,
    config: Config,
}

impl Widget {
    /// Level for a window's percentage given the configured thresholds.
    fn level(&self, used_pct: f32) -> Level {
        Level::from_pct(used_pct, self.config.warn_threshold, self.config.critical_threshold)
    }
}

// M2/M4: implement `gpui::Render` for `Widget`.
//
// Bars view (default) — gpui-component `ProgressBar` per window, colored by
// `theme::level_color(self.level(pct))`, over a `theme::scrim()` card:
//
//   let snap = self.shared.lock().unwrap().clone();
//   v_flex().gap_2().p_3()
//     .child(/* "Claude · Max 5× · {provenance}" */)
//     .child(bar_row("5H", snap.five_hour, ...))   // primary, larger
//     .child(bar_row("7D", snap.seven_day, ...))   // secondary
//     .child(/* "today {tokens} · {top model}" */)
//
// Gauge view (M4) — two `ProgressCircle::new(id).value(pct).color(..)` rings.
// View toggle + right-click menu (backdrop switch, refresh, scale, quit) +
// drag-to-move also land in M4.
