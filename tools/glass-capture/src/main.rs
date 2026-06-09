// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! Capture a screen rectangle via DXGI Desktop Duplication → PNG.
//!
//! This captures the *composited* desktop, so it sees gpui's DirectComposition
//! flip-swapchain window AND any DWM Mica/Acrylic backdrop — unlike GDI
//! `BitBlt`/`CopyFromScreen`, which return black for such windows.
//!
//! Usage: `glass-capture <left> <top> <width> <height> <out.png>`
//! (coords are virtual-desktop pixels, e.g. from GetWindowRect)
//! Prints `mean_luma` and `stddev_luma` over the rect (a flat dead panel has
//! stddev ≈ 0; a real frosted/see-through panel over varied content does not).

use core::ffi::c_void;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIOutput1, IDXGIResource, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect, SetCursorPos};

fn err(msg: &str) -> windows::core::Error {
    windows::core::Error::new(windows::core::HRESULT(-1), msg)
}

fn main() {
    // Match the texture's physical-pixel space so a 4K/scaled monitor lines up.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let a: Vec<String> = std::env::args().collect();
    // Two forms:  <hwnd> <pad> <out.png>   |   <left> <top> <width> <height> <out.png>
    let (rect, out) = if a.len() == 4 {
        let hwnd = HWND(a[1].parse::<isize>().expect("hwnd") as *mut c_void);
        let pad: i32 = a[2].parse().unwrap_or(20);
        let mut r = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut r).expect("GetWindowRect") };
        let w = (r.right - r.left + 2 * pad).max(1) as u32;
        let h = (r.bottom - r.top + 2 * pad).max(1) as u32;
        ((r.left - pad, r.top - pad, w, h), a[3].clone())
    } else if a.len() == 6 {
        (
            (
                a[1].parse().expect("left"),
                a[2].parse().expect("top"),
                a[3].parse().expect("width"),
                a[4].parse().expect("height"),
            ),
            a[5].clone(),
        )
    } else {
        eprintln!("usage: glass-capture <hwnd> <pad> <out.png>");
        eprintln!("   or: glass-capture <left> <top> <width> <height> <out.png>");
        std::process::exit(2);
    };

    match unsafe { capture(rect, &out) } {
        Ok((mean, stddev)) => {
            println!("OK saved={out} mean_luma={mean:.1} stddev_luma={stddev:.2}")
        }
        Err(e) => {
            eprintln!("ERR {e:?}");
            std::process::exit(1);
        }
    }
}

unsafe fn capture(rect: (i32, i32, u32, u32), out: &str) -> windows::core::Result<(f64, f64)> {
    let (l, t, w, h) = rect;

    // D3D11 device.
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut level = D3D_FEATURE_LEVEL::default();
    D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_FLAG(0),
        None,
        D3D11_SDK_VERSION,
        Some(&mut device),
        Some(&mut level),
        Some(&mut context),
    )?;
    let device = device.unwrap();
    let context = context.unwrap();
    let dxgi_dev: IDXGIDevice = device.cast()?;
    let adapter = dxgi_dev.GetAdapter()?;

    // Pick the monitor (output) that contains the rect's center, and remember
    // its virtual-desktop origin so we can convert to output-local pixels.
    let cx = l + (w as i32) / 2;
    let cy = t + (h as i32) / 2;
    let mut chosen: Option<(IDXGIOutput1, i32, i32, i32, i32)> = None; // out1, ox, oy, ow, oh
    let mut idx = 0u32;
    while let Ok(output) = adapter.EnumOutputs(idx) {
        let od = output.GetDesc()?;
        let d = od.DesktopCoordinates;
        if cx >= d.left && cx < d.right && cy >= d.top && cy < d.bottom {
            chosen = Some((
                output.cast()?,
                d.left,
                d.top,
                d.right - d.left,
                d.bottom - d.top,
            ));
            break;
        }
        idx += 1;
    }
    let (output1, ox, oy, ow, oh) =
        chosen.ok_or_else(|| err("no monitor contains the requested rect"))?;
    eprintln!("dbg: monitor origin=({ox},{oy}) size={ow}x{oh}");
    let dupl = output1.DuplicateOutput(&device)?;

    // Force real presents ON THE TARGET MONITOR by jiggling the cursor there,
    // and accept a frame only once LastPresentTime != 0 (a real present has
    // updated the duplication surface — the initial surface can be black).
    let mut start = POINT::default();
    let _ = GetCursorPos(&mut start);
    let (mcx, mcy) = (ox + ow / 2, oy + oh / 2);
    let mut staging: Option<ID3D11Texture2D> = None; // confirmed-presented frame
    let mut fallback: Option<ID3D11Texture2D> = None; // last frame, even if 0
    for i in 0..120 {
        let _ = SetCursorPos(mcx + (i % 5) - 2, mcy + (i % 3) - 1);
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        match dupl.AcquireNextFrame(200, &mut info, &mut res) {
            Ok(()) => {
                if let Some(r) = res.as_ref() {
                    let frame: ID3D11Texture2D = r.cast()?;
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    frame.GetDesc(&mut desc);
                    let sdesc = D3D11_TEXTURE2D_DESC {
                        Usage: D3D11_USAGE_STAGING,
                        BindFlags: 0,
                        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                        MiscFlags: 0,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        ..desc
                    };
                    let mut s: Option<ID3D11Texture2D> = None;
                    device.CreateTexture2D(&sdesc, None, Some(&mut s))?;
                    let s = s.unwrap();
                    context.CopyResource(&s, &frame);
                    if info.LastPresentTime != 0 {
                        staging = Some(s);
                    } else {
                        fallback = Some(s);
                    }
                }
                let _ = dupl.ReleaseFrame();
                if staging.is_some() {
                    break;
                }
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
            Err(e) => return Err(e),
        }
    }
    let _ = SetCursorPos(start.x, start.y);
    let staging = staging
        .or(fallback)
        .ok_or_else(|| err("no frame acquired"))?;

    // Map and read the requested sub-rect (output-local, BGRA → RGBA), clamped.
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
    let pitch = m.RowPitch as usize;
    let base = m.pData as *const u8;
    let rl = l - ox;
    let rt = t - oy;
    eprintln!("dbg: pitch={pitch} read_origin=({rl},{rt}) read_size={w}x{h}");
    let mut rgba: Vec<u8> = Vec::with_capacity((w * h * 4) as usize);
    let (mut sum, mut sum2, mut n) = (0f64, 0f64, 0f64);
    for y in 0..h as i32 {
        let sy = (rt + y).clamp(0, oh - 1);
        let row = base.add(sy as usize * pitch);
        for x in 0..w as i32 {
            let sx = (rl + x).clamp(0, ow - 1);
            let p = row.add(sx as usize * 4);
            let b = *p;
            let g = *p.add(1);
            let r = *p.add(2);
            rgba.extend_from_slice(&[r, g, b, 255]);
            let luma = 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
            sum += luma;
            sum2 += luma * luma;
            n += 1.0;
        }
    }
    context.Unmap(&staging, 0);

    let img = image::RgbaImage::from_raw(w, h, rgba).expect("buffer size");
    img.save(out).map_err(|e| err(&format!("save: {e}")))?;

    let mean = sum / n;
    let var = (sum2 / n - mean * mean).max(0.0);
    Ok((mean, var.sqrt()))
}
