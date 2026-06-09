// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! Rasterize `assets/icon.svg` → `assets/icon.ico` at 16/32/48/64/256 px.

use resvg::{tiny_skia, usvg};

// Resolved relative to this crate so it works from any clone location.
const BASE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/usage-widget/assets"
);

fn main() {
    let svg = std::fs::read_to_string(format!("{BASE}/icon.svg")).expect("read icon.svg");
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).expect("parse svg");
    let size = tree.size();
    let (w0, h0) = (size.width(), size.height());

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for s in [256u32, 64, 48, 32, 16] {
        let mut pm = tiny_skia::Pixmap::new(s, s).expect("pixmap");
        let scale = (s as f32 / w0).min(s as f32 / h0);
        let tx = tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&tree, tx, &mut pm.as_mut());
        let data = unpremultiply(pm.data().to_vec());
        let img = ico::IconImage::from_rgba_data(s, s, data);
        dir.add_entry(ico::IconDirEntry::encode(&img).expect("encode"));
    }
    let f = std::fs::File::create(format!("{BASE}/icon.ico")).expect("create ico");
    dir.write(f).expect("write ico");
    println!("wrote {BASE}/icon.ico (16/32/48/64/256)");
}

/// tiny-skia outputs premultiplied alpha; .ico wants straight RGBA.
fn unpremultiply(mut d: Vec<u8>) -> Vec<u8> {
    for px in d.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 {
            for c in 0..3 {
                px[c] = (((px[c] as u32) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    d
}
