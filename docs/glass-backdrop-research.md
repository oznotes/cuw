# Frosted-glass backdrop — HISTORICAL research (companion-window prototype, SHELVED)

## 🗄️ HISTORICAL 2026-06-09 — preserved research; the prototype documented here was SHELVED

**Read this first — what shipped vs. what this document is:**

- **The widget currently ships CLEAR-ONLY see-through glass.** A transparent gpui window (`WindowBackgroundAppearance::Transparent`) + a local transparent override on the gpui-component `Root` (so its opaque `cx.theme().background` paint doesn't black out the window) + a dark legibility scrim (`theme::panel_bg(opacity)`). There is **no frosted blur** in the shipped product.
- **The companion-window DWM-backdrop approach documented below was a PROTOTYPE, and it was SHELVED.** It was built and partly verified, but it **composites inconsistently** — it frosts over the wallpaper yet goes dark over plain windows — and the unowned-sibling z-order/lifetime management is fragile. It is **not** what ships.
- **This document is preserved as the technical map for a FUTURE proper-frosted-glass phase.** Every technical finding, code snippet, reference, and the DXGI verification method below remains valid and is kept intact for whoever picks that work up. Treat the past-tense "we tried / it composited / DWM stored the request" notes as a record of what the prototype did, not as a description of current behavior.

What the shelved prototype attempted (THREE coupled changes; any one alone failed):
1. **Companion backdrop window** (`win/backdrop_window.rs`): a separate, **unowned**, **non-layered** (`WS_EX_NOACTIVATE|WS_EX_TRANSPARENT|WS_EX_TOOLWINDOW`, NO `WS_EX_LAYERED`) borderless `WS_POPUP` hosting the DWM backdrop, with `DwmExtendFrameIntoClientArea(-1)` (a *frameless* window needs it) + `DWMWA_SYSTEMBACKDROP_TYPE`, glued beneath the widget each frame, rounded via `DWMWA_WINDOW_CORNER_PREFERENCE`.
2. **THE blocker it had to clear:** `gpui-component`'s `Root::render` paints the **whole window opaque** with `cx.theme().background` (`root.rs:526`) — the "black main window" on top of everything. The fix (set the widget's `Root` instance background to transparent; widget window = `WindowBackgroundAppearance::Transparent`) is what the shipped clear-only build retains.
3. **Verification tool** `tools/glass-capture` — DXGI Desktop Duplication → PNG (GDI returns black for this window; must be DPI-aware for scaled monitors). This ended the "false victory" cycle and remains the recommended verifier for the future phase.

**Glass styles the prototype exposed** (cycled via the Details "glass" chip): `clear` (Transparent → `DWMSBT_NONE`, real see-through, `opacity` = genuine transparency) · `acrylic` (`DWMSBT_TRANSIENTWINDOW`, frosted blur) · `mica` / `mica-alt` (wallpaper tint). Of these, only **clear** survived into the shipped product. gpui's own `Blurred`/`MicaBackdrop` were both dead on build 26200 (legacy accent nerfed; DWM ignores backdrop on the NOREDIRECTIONBITMAP window) — the companion + transparent Root was the only thing that frosted at all, but its inconsistent compositing is why it was shelved.

Everything below is the original investigation (kept for the record / future phase).

---

**Status:** ~~OPEN~~ ~~RESOLVED~~ **SHELVED — prototype only; widget ships clear-only. See header above.**
**Date:** 2026-06-09 · Windows 11 build **26200** · Rust + gpui/gpui-component widget.
**Evidence screenshot:** [`docs/glass-broken-2026-06-09.png`](docs/glass-broken-2026-06-09.png)
**This report was produced by a 5-agent root-cause investigation; every claim below was verified against the gpui source in `~/.cargo`.**

---

## ⚠️ UPDATE 2026-06-09 — Step 1 (`Blurred`) was ATTEMPTED and FAILED visually. Read this first.

We applied the lead fix (set `window_background: Blurred`, route the cycle through `window.set_background_appearance`, disabled the companion) and the user verified on build **26200**: **still flat — no see-through, no blur.** So the "lead cause" fix below (Step 1) does **NOT** work on this machine.

**Why:** gpui's `Blurred` uses the legacy **`ACCENT_ENABLE_ACRYLICBLURBEHIND`** (user32 accent). Microsoft **deprecated/throttled** that API on Win10 1903+/Win11 — on modern builds it commonly degrades to a **flat tint with no blur**, especially for **always-on-top / unfocused** windows (exactly this widget). So both built-in gpui paths are dead here: `Blurred` → flat (nerfed accent), `MicaBackdrop` → ignored (zed#38995, `NOREDIRECTIONBITMAP`).

**What IS still known-good (use these):**
1. **The widget window CAN be transparent.** Earlier tests showed the *actual desktop* visible through it (icons, crisp). So a transparent-window approach can show *something* behind — which means the **DIY-blur fallback is viable and is now the recommended primary path** (render the frost into the widget's own background; the transparent window shows it). It's the only option fully under our control AND verifiable without the user's eyes (dump the texture to PNG before upload).
2. **The companion's modern DWM acrylic appeared to composite.** In the evidence screenshot a **gray frosted halo** showed in the window's shadow margin (though it's ambiguous whether that was the companion's `DWMSBT_TRANSIENTWINDOW` acrylic or gpui's own drop-shadow). If it was the companion, a **de-fanged companion** (Step 2: drop `WS_EX_LAYERED`, drop the frame-extend, drop `SetWindowRgn`, fix z-order, modern `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW`) with the widget fully transparent (opacity 0) on top could frost the whole panel. Cheap to try; **verify with DXGI before trusting it.**

**Recommended order for the next agent:**
- **(A)** Build the **DXGI Desktop Duplication verifier first** (code in "How to VERIFY" below). Nothing should be called "working" again without it — that mistake cost this whole project a day.
- **(B)** Try the **de-fanged companion** (Step 2) — quick, possibly already 90% there. Verify with (A).
- **(C)** If (B) doesn't cleanly frost, build the **DIY blur** (Fallback plan) — guaranteed, verifiable, the safe landing.

**Current code state:** `src/ui/mod.rs` is on `Blurred` via a new `appearance_for(Backdrop)` helper; the companion is **disabled** (`let backdrop = None`) but its files (`win/backdrop_window.rs`, `win/backdrop.rs`) still exist for Step 2 reuse. `Cargo.toml` still has the Gdi/Controls/LibraryLoader/Dwm features. Config default is Acrylic/opacity — set whatever you need for testing.

> NOTE: Step 1 below is left intact for the record, but it is **disproven on build 26200**. Start from Step 2 / the Fallback.

---

## Independent 2nd-agent review (2026-06-09) — converged on the same root cause

A separate agent reviewed the prototype code live (including DWM read-back) and independently reached the same conclusion. Confirmations + additions (historical — describes the shelved prototype):

- **DWM read-back proved OS support:** the companion had `DWMWA_SYSTEMBACKDROP_TYPE = 3` (`DWMSBT_TRANSIENTWINDOW` = Desktop Acrylic); transparency effects ON; build 26200; config Acrylic/0.15. **This was a composition-architecture problem, NOT missing OS support** — which is consistent with the inconsistent compositing (wallpaper vs. plain windows) that ultimately got the approach shelved.
- **User-observed:** disabling the companion made the gray "second background" **disappear** — confirming that halo *was* the companion's Acrylic compositing. The Acrylic rendered, but it was trapped in a layered/owned popup instead of being the widget's real background.
- **[P1] `WS_EX_LAYERED` is wrong** (`backdrop_window.rs:106`) + `SetLayeredWindowAttributes` (`:123`): backdrop lives inside a separate *layered* popup, not as the GPUI widget's background.
- **[P1] Owned-popup z-order fight:** companion created *owned* by the GPUI window (`backdrop_window.rs:116`), which is pinned topmost first (`ui/mod.rs:90`), then fights Windows' owned-popup z-order with `SetWindowPos` (`backdrop_window.rs:217`). Make it an **unowned, non-layered sibling** with manual z-order/lifetime.
- **[P2] Material correctness:** **Mica is opaque/wallpaper-tinted — it cannot "see behind."** Default to **one Acrylic mode**; hide/rename Mica/MicaAlt.
- **[P2] DWM HRESULTs swallowed** (`backdrop_window.rs:170,180`) — add diagnostics so a future failure isn't a silent "theme broken."
- **[P3] (fixed)** stale `theme.rs` comment said default 0.80; actual 0.15. ✔

**2nd agent's recommended plan:**
1. **Diagnostics first:** log backdrop type, opacity, build, transparency setting, both HWND ex-styles, and **every DWM HRESULT**.
2. **Test the cleaner route:** set `GPUI_DISABLE_DIRECT_COMPOSITION=1` *early* and apply DWM Acrylic to the **actual GPUI window**; if perf is fine, **delete the companion.** (The prior agent saw black with this env var but had NOT combined it with applying a real DWM backdrop to the now-redirected window — re-test properly with diagnostics.)
3. **If keeping the companion:** **unowned, non-layered sibling** — remove `WS_EX_LAYERED` + `SetLayeredWindowAttributes`; manage z-order/lifetime manually (just below the topmost widget).
4. **Product decision:** one Acrylic "glass" mode; hide Mica/MicaAlt.
5. **Verify over high-contrast MOVING windows**, with a real capture (DXGI / `Win+Shift+S`), never GDI.

**Microsoft references (user-supplied):**
- Materials (Acrylic = frosted/semi-transparent; Mica = opaque/wallpaper-tinted): https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/materials
- DWM_SYSTEMBACKDROP_TYPE: https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type
- Window Features (owned-window z-order constraints): https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features
- Extended Window Styles (`WS_EX_NOREDIRECTIONBITMAP`/`WS_EX_LAYERED`/`WS_EX_TRANSPARENT`): https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles

**Both reviews → next agent's best first move:** build diagnostics + the DXGI verifier, then **try route #2 (`GPUI_DISABLE_DIRECT_COMPOSITION=1` + DWM Acrylic on the real GPUI window)** — cleanest if it works. If still black/opaque, **de-fang the companion into an unowned non-layered sibling** (route #3) — we have direct evidence its Acrylic composites. DIY blur is the guaranteed last resort.

---

## TL;DR for the next agent

1. **Delete the companion backdrop window** (`src/win/backdrop_window.rs`) — it was an unnecessary, fragile detour and is the wrong architecture for this gpui revision.
2. **The real bug is one enum value.** The widget sets `WindowBackgroundAppearance::Transparent` (gpui accent state 2 = plain transparency, **no blur**). The pinned gpui fork already does **real acrylic blur** on the widget's own HWND via `WindowBackgroundAppearance::Blurred` (accent state 4 = `ACCENT_ENABLE_ACRYLICBLURBEHIND`). The project just never tried `Blurred`. This path is the **user32 accent API**, independent of the DWM `SYSTEMBACKDROP_TYPE` route that zed#38995 blocks on `NOREDIRECTIONBITMAP` windows.
3. **Fix:** `Transparent` → `Blurred`; route the glass-cycle through `window.set_background_appearance(...)`; delete the companion + its wiring. ~1 hour incl. cleanup. Steps below.
4. **You cannot verify this with a normal screenshot.** GDI `BitBlt`/`CopyFromScreen` returns **black** for gpui's DirectComposition window. Use **DXGI Desktop Duplication** (code below) or the **user's own eyes** (`Win + Shift + S` captures it correctly). Do not repeat the prior agent's mistake of trusting GDI screenshots.
5. **Known caveat:** legacy `ACCENT_ENABLE_ACRYLICBLURBEHIND` acrylic can degrade to a **flat tint when the window is inactive/unfocused** on some Win11 22H2+ builds. An always-on-top, non-activating widget can land in that state. If the blur is dead specifically when unfocused, use the Fallback plan.

---

## Observed symptom (from the screenshot)

A dark, **opaque-looking** rounded panel with all the widget content (5H/7D bars, projects, heatmap, settings row), surrounded by a thin **light-gray frosted halo**. That halo is the only place any backdrop shows — it leaks through the window's transparent *shadow margin*. The panel body itself shows **no blur and no see-through**. Cycling glass material or opacity changes nothing visible. This is consistent with: the gpui window contributing *no frost* (`Transparent` = no blur), and the companion window being suppressed (see root cause).

---

## What works / what's confirmed

- The widget renders, drags, persists position, cycles view/glass/opacity, shows live usage data — `src/ui/mod.rs`.
- gpui's DirectComposition path clears the swapchain to straight alpha 0 for any non-`Opaque` appearance (`…/gpui_windows/src/directx_renderer.rs:314-317`: `Opaque => [1.0;4], _ => [0.0;4]`). The widget's transparent pixels are real on the DComp path — **keep DirectComposition on; do NOT set `GPUI_DISABLE_DIRECT_COMPOSITION=1`** (that makes the HWND swapchain ignore alpha → opaque black).
- **Decisive, code-verified finding** — the pinned gpui fork (`zed-a70e2ad075855582/f99fe5d`) already implements and publicly exposes legacy acrylic blur on its own NOREDIRECTIONBITMAP HWND:
  - enum `WindowBackgroundAppearance { Opaque, Transparent, Blurred, MicaBackdrop, MicaAltBackdrop }` — `…/gpui/src/platform.rs:1693-1712`.
  - `Window::set_background_appearance(&self, WindowBackgroundAppearance)` — `…/gpui/src/window.rs:2326`.
  - Windows impl — `…/gpui_windows/src/window.rs:831-856`: `Blurred => set_window_composition_attribute(hwnd, Some((0,0,0,0)), 4)` (accent state 4 = `ACCENT_ENABLE_ACRYLICBLURBEHIND`); `MicaBackdrop`/`MicaAltBackdrop` => `dwm_set_window_composition_attribute(hwnd, 2|4)`; `Transparent => state 2` (no blur — what the app currently sets).
- Reference implementations (Tauri `window-vibrancy`, wezterm) apply their backdrop to a **plain, redirected, framed** top-level window and **never set `WS_EX_LAYERED`**.

---

## Root cause — ranked

### LEAD CAUSE (~80%): app uses `Transparent` (no blur) instead of the built-in `Blurred`; the companion is the wrong architecture

The widget sets `window_background: WindowBackgroundAppearance::Transparent` (`src/ui/mod.rs`), accent state 2 — plain transparency, **zero blur** — so the gpui window contributes no frost. The companion window was added to supply frost externally but is fragile-to-dead on this window type (defects below) and additionally killed by `WS_EX_LAYERED`. Meanwhile gpui already wires working acrylic onto the widget's own HWND via `Blurred`. The renderer clears transparent for both `Transparent` and `Blurred` (`directx_renderer.rs:314`), so the only difference between "flat see-through" and "frost" is which accent state gpui sets — and the app set the wrong one.

### Why the companion fails even if kept (each ~70% likely individually fatal)

- **(a) `WS_EX_LAYERED` + `SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA)` suppresses the DWM backdrop.** `backdrop_window.rs:106,123`. With `bAlpha=255` ("opaque") and a NULL class brush painting nothing, you get an opaque empty layer over the backdrop; the system-backdrop frame layer is never sampled into output. `window-vibrancy`/wezterm never set `WS_EX_LAYERED`.
- **(b) `DWMWA_SYSTEMBACKDROP_TYPE` needs a real frame; the companion is a frameless `WS_POPUP`.** DWM paints Mica/Acrylic in the non-client frame region; a borderless popup has none, so `DwmExtendFrameIntoClientArea(-1…)` spreads nothing. This is why window-vibrancy always targets the main *framed* window (same failure class as zed#38995).
- **(c) `SetWindowRgn` rounded region can clip the frame DWM would paint.** `backdrop_window.rs:228-232`. Use `DWMWA_WINDOW_CORNER_PREFERENCE` instead of a GDI region.

### Lower-probability / masking
- z-order fragility (~10%): companion never explicitly topmost + `SWP_NOOWNERZORDER` in `sync_to`. Moot once the companion is deleted.
- dark immersive tint + 0.15 scrim over a dark wallpaper reads as "dim flat" (~5%). Test with a bright/busy wallpaper and `opacity = 0.0`.

---

## The fix plan (ordered, concrete — `windows` crate v0.58)

### Step 1 (PRIMARY — do this first, in isolation): use gpui's built-in `Blurred`; delete the companion

**1a.** In `src/ui/mod.rs`, `window_options()`:
```rust
window_background: gpui::WindowBackgroundAppearance::Blurred, // was ::Transparent
```
`Blurred` = acrylic blur-behind ("see behind, blurred"). `MicaBackdrop`/`MicaAltBackdrop` = wallpaper-tinted, for the cycle.

**1b.** Rewire `cycle_backdrop` to call gpui's setter on the `Window` (it already receives `window`):
```rust
fn cycle_backdrop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.config.backdrop = next_backdrop(self.config.backdrop);
    let appearance = match self.config.backdrop {
        Backdrop::Acrylic | Backdrop::Transparent => WindowBackgroundAppearance::Blurred,
        Backdrop::Mica | Backdrop::Opaque        => WindowBackgroundAppearance::MicaBackdrop,
        Backdrop::MicaAlt                        => WindowBackgroundAppearance::MicaAltBackdrop,
    };
    window.set_background_appearance(appearance);
    let _ = self.config.save();
    cx.notify();
}
```

**1c.** Delete the companion entirely:
- Delete `src/win/backdrop_window.rs`.
- Remove the `backdrop: Option<BackdropWindow>` field, the `BackdropWindow::new(...)` call, the `Widget::new` `backdrop` param, and the per-frame `bd.sync_to(...)` in `render`.
- Trim/delete `src/win/backdrop.rs` (keep `windows_build_number`/`supports_mica` only if still referenced). Remove the `mod backdrop_window;` (and `mod backdrop;` if unused) from `src/win/mod.rs`. Remove `win::raw_hwnd` if now unused.
- In `Cargo.toml`, drop now-unused `windows` features: `Win32_Graphics_Gdi`, `Win32_System_LibraryLoader`, `Win32_UI_Controls` (and Dwm/SystemInformation/Wdk if nothing else uses them). Keep `Win32_Foundation` + `Win32_UI_WindowsAndMessaging` (used by topmost/drag).

**1d.** Keep the legibility scrim `panel_bg(self.config.opacity)`. For the first verification pass set `opacity = 0.0` to judge the raw frost, then restore ~0.12–0.2.

### Step 2 (only if Step 1's acrylic is throttled when unfocused on 26200): de-fang the companion for true DWM Mica
- Drop `WS_EX_LAYERED` (`let ex = WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW;`) and delete `SetLayeredWindowAttributes`. Click-through survives via `WS_EX_TRANSPARENT` + the existing `WM_NCHITTEST → HTTRANSPARENT`.
- Replace `SetWindowRgn` with DWM rounding: `DWMWA_WINDOW_CORNER_PREFERENCE (33) = DWMWCP_ROUND (2)`.
- Drop `DwmExtendFrameIntoClientArea` (modern `SYSTEMBACKDROP_TYPE` fills the client itself; window-vibrancy omits it).
- Make the companion explicitly `HWND_TOPMOST` after `ShowWindow`, and drop `SWP_NOOWNERZORDER` in `sync_to`.
- Still fragile (Mica on a frameless popup is build-dependent) — that's why it's the fallback.

---

## How to VERIFY without the user's eyes

GDI `BitBlt`/`CopyFromScreen` returns **black** on the DComp window — unusable. `PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT=0x2)` captures the widget's *own* UI (good "did it render" smoke test) but **cannot show frost** (frost is a multi-window DWM composite). `DwmGetWindowAttribute` read-back proves DWM *stored* the request, not that it *rendered*.

**Authoritative method: DXGI Desktop Duplication** of the composited desktop (includes DComp flip-swapchains + DWM acrylic), cropped to the widget rect, compared over a bright vs black background. Glass is live iff `mean(luma)` over a bright bg ≫ over a black bg AND local `stddev > ~few` (the blur). A dead panel yields uniform low luma, `stddev ≈ 0`.

Add for verification (`Cargo.toml`):
```toml
windows = { version = "0.58", features = [
  "Win32_Foundation", "Win32_Graphics_Dxgi", "Win32_Graphics_Dxgi_Common",
  "Win32_Graphics_Direct3D", "Win32_Graphics_Direct3D11",
  # …existing features…
] }
image = "0.25"
```

```rust
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGIOutput1, IDXGIResource, DXGI_OUTDUPL_FRAME_INFO};

/// Capture the composited desktop; save `rect` (l,t,w,h px) as PNG.
/// Returns (mean_luma, stddev_luma) so a test can assert glass is live.
pub unsafe fn capture_rect_png(rect: (i32,i32,u32,u32), out: &str)
    -> windows::core::Result<(f64,f64)>
{
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(),
        D3D11_CREATE_DEVICE_FLAG(0), None, D3D11_SDK_VERSION,
        Some(&mut device), Some(&mut D3D_FEATURE_LEVEL::default()), Some(&mut context))?;
    let device = device.unwrap(); let context = context.unwrap();

    let dxgi_dev: IDXGIDevice = device.cast()?;
    let adapter = dxgi_dev.GetAdapter()?;
    let output = adapter.EnumOutputs(0)?;                 // 0 = primary monitor
    let output1: IDXGIOutput1 = output.cast()?;
    let dupl = output1.DuplicateOutput(&device)?;

    // First AcquireNextFrame is often metadata-only — retry until real pixels.
    // The desktop must CHANGE to produce a frame: nudge cursor / toggle bg.
    let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
    let mut res: Option<IDXGIResource> = None;
    for _ in 0..16 {
        if let Some(r) = res.take() { drop(r); let _ = dupl.ReleaseFrame(); }
        dupl.AcquireNextFrame(500, &mut info, &mut res)?;  // 500 ms timeout
        if info.LastPresentTime != 0 { break; }
    }
    let frame: ID3D11Texture2D = res.as_ref().unwrap().cast()?;

    // Acquired tex is GPU-only -> CopyResource into CPU-readable STAGING tex.
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    frame.GetDesc(&mut desc);
    let sdesc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING, BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, ..desc
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    device.CreateTexture2D(&sdesc, None, Some(&mut staging))?;
    let staging = staging.unwrap();
    context.CopyResource(&staging, &frame);

    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
    let (l,t,w,h) = rect;
    let pitch = m.RowPitch as usize;          // NOTE: pitch != w*4, stride by it
    let base = m.pData as *const u8;
    let mut rgba = Vec::with_capacity((w*h*4) as usize);
    let (mut sum, mut sum2, mut n) = (0f64,0f64,0f64);
    for y in 0..h as i32 {
        let row = base.add(((t+y) as usize)*pitch);
        for x in 0..w as i32 {
            let p = row.add(((l+x) as usize)*4);                 // BGRA
            let (b,g,r,a) = (*p,*p.add(1),*p.add(2),*p.add(3));
            rgba.extend_from_slice(&[r,g,b,a]);
            let luma = 0.2126*r as f64 + 0.7152*g as f64 + 0.0722*b as f64;
            sum += luma; sum2 += luma*luma; n += 1.0;
        }
    }
    context.Unmap(&staging, 0);
    drop(frame); let _ = dupl.ReleaseFrame();
    image::save_buffer(out, &rgba, w, h, image::ColorType::Rgba8)
        .map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), format!("{e}")))?;
    let mean = sum/n; let var = (sum2/n - mean*mean).max(0.0);
    Ok((mean, var.sqrt()))
}
```

**Eye-free verdict protocol:** (1) spawn a borderless maximized **bright white** window behind the widget → `capture_rect_png(widget_rect, "glass_white.png")`. (2) swap it to **black** → `capture_rect_png(...,"glass_black.png")`. (3) Glass is live iff `mean(white) − mean(black)` is large AND `stddev > ~few`. Gotchas: one `ReleaseFrame` per successful `AcquireNextFrame`; retry until `LastPresentTime != 0` (toggle the bg to force a frame); format is BGRA, swap to RGBA; stride by `RowPitch`. Cheap structural check after any Step-2 fix: assert `GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED == 0`.

---

## Fallback plan (if both `Blurred` and de-layered companion Mica are dead on 26200)

Render the frost yourself into the widget's own `img()` background — fully controllable, trivially verifiable (dump the texture to PNG before upload):
- **Wallpaper-only (Mica semantics, cheap):** read wallpaper via `SystemParametersInfoW(SPI_GETDESKWALLPAPER, …)` (or `HKCU\Control Panel\Desktop\WallPaper`, or `…\Microsoft\Windows\Themes\TranscodedWallpaper`), crop to the widget's screen rect (account for `WallpaperStyle` fit + DPI), Gaussian-blur, feed to gpui `img(ImageSource)`; re-crop on move (`save_position_if_moved`).
- **True see-behind (expensive):** the DXGI duplication capture above, downsample+blur, upload throttled on move (exclude your own window from the capture).
- **Not recommended:** `WS_CHILD` overlay inside gpui's HWND (inherits the DComp issue) or a fresh sibling overlay (reintroduces the companion problems).

---

## Key files & current state

- `crates/usage-widget/src/ui/mod.rs` — `window_background: Transparent` (→ `Blurred`); `cycle_backdrop` (→ `window.set_background_appearance`); companion wiring (delete); scrim `panel_bg(opacity)` (keep).
- `crates/usage-widget/src/win/backdrop_window.rs` — the companion. **Delete** under Step 1 (or surgically fix under Step 2: `WS_EX_LAYERED`, `SetLayeredWindowAttributes`, `DwmExtendFrameIntoClientArea(-1…)`, `SetWindowRgn`, `SWP_NOOWNERZORDER`).
- `crates/usage-widget/src/win/backdrop.rs` — `windows_build_number`/`supports_mica`; trim/delete if unreferenced after Step 1.
- `crates/usage-widget/src/win/topmost.rs` — widget `HWND_TOPMOST` pin (relevant only to Step 2 z-order).
- `crates/usage-widget/src/ui/theme.rs` — `panel_bg` scrim (keep).
- `crates/usage-widget/Cargo.toml` — `windows = "0.58"`; drop Gdi/LibraryLoader/Controls features after Step 1; add Dxgi/Direct3D11 + `image` for verification only.
- **Ground-truth gpui source (proves the built-in path):** `~/.cargo/git/checkouts/zed-a70e2ad075855582/f99fe5d/crates/gpui_windows/src/window.rs:831-856` (+ `set_window_composition_attribute` `:1517-1556`); public wrapper `…/crates/gpui/src/window.rs:2326`; enum `…/crates/gpui/src/platform.rs:1693-1712`; transparent clear `…/crates/gpui_windows/src/directx_renderer.rs:314-317`. (Crate is `gpui_windows`, not `gpui/src/platform/windows`.)

## Build / run notes
- Build env: `Enter-VsDevShell` (`…/2022/Community/Common7/Tools/Microsoft.VisualStudio.DevShell.dll`) + the bundled CMake/Ninja on PATH (`…/Common7/IDE/CommonExtensions/Microsoft/CMake/{CMake/bin,Ninja}`). Toolchain rustc 1.96.0 (pinned). Build with `--locked`.
- Set `$env:CLAUDE_CODE_VERSION` before launching (the OAuth usage fetch needs a `User-Agent: claude-code/<version>`).
- Installed copy + start-on-login: `%LOCALAPPDATA%\ClaudeUsageWidget\claude-usage-widget.exe`. Live config: `%APPDATA%\claude-usage\widget-config.json`.
- **Verify with the user's eyes** (`Win + Shift + S`) or DXGI — never GDI screenshots.

## References
- SetLayeredWindowAttributes (bAlpha=255 ⇒ opaque): https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setlayeredwindowattributes
- tauri-apps/window-vibrancy (no WS_EX_LAYERED): https://github.com/tauri-apps/window-vibrancy
- wezterm PR #3528 (system backdrop): https://github.com/wezterm/wezterm/pull/3528
- DWM_SYSTEMBACKDROP_TYPE: https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type
- Apply Mica in Win32: https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/ui/apply-mica-win32
- System backdrops (Mica/Acrylic): https://learn.microsoft.com/en-us/windows/apps/develop/ui/system-backdrops
- Enabling Backdrop Blur (yvt.jp): https://notes.yvt.jp/Desktop-Apps/Enabling-Backdrop-Blur/
- Capturing a Window (PrintWindow vs BitBlt vs DXGI): https://learn.microsoft.com/en-us/answers/questions/801244/capturing-a-window
- IDXGIOutputDuplication::AcquireNextFrame: https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-acquirenextframe
- zed#38995 (DWM ignores SYSTEMBACKDROP_TYPE on NOREDIRECTIONBITMAP windows): https://github.com/zed-industries/zed/issues/38995
