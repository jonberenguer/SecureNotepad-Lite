use image::{RgbaImage, Rgba};
use std::io::Cursor;
use std::path::PathBuf;

// ─── Pixel drawing helpers ────────────────────────────────────────────────────

fn fill_rect(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: Rgba<u8>) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    for row in 0..h {
        for col in 0..w {
            let (px, py) = (x + col, y + row);
            if px >= 0 && py >= 0 && px < iw && py < ih {
                img.put_pixel(px as u32, py as u32, c);
            }
        }
    }
}

fn fill_circle(img: &mut RgbaImage, cx: i32, cy: i32, r: i32, c: Rgba<u8>) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                let (px, py) = (cx + dx, cy + dy);
                if px >= 0 && py >= 0 && px < iw && py < ih {
                    img.put_pixel(px as u32, py as u32, c);
                }
            }
        }
    }
}

fn fill_rounded_rect(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, r: i32, c: Rgba<u8>) {
    fill_rect(img, x + r, y,     w - 2 * r, h,         c);
    fill_rect(img, x,     y + r, w,         h - 2 * r, c);
    fill_circle(img, x + r,     y + r,     r, c);
    fill_circle(img, x + w - r, y + r,     r, c);
    fill_circle(img, x + r,     y + h - r, r, c);
    fill_circle(img, x + w - r, y + h - r, r, c);
}

// ─── Lock icon (drawn at 256 × 256, scaled for smaller sizes) ─────────────────

fn draw_icon(size: u32) -> RgbaImage {
    let mut img = RgbaImage::new(size, size);
    let s = size as f32;

    let bg     = Rgba([22u8,  22,  28,  255]); // #16161c — app dark background
    let purple = Rgba([124u8, 106, 247, 255]); // #7c6af7 — app accent

    for p in img.pixels_mut() { *p = bg; }

    let cx      = size as i32 / 2;
    let arc_cy  = (0.305 * s) as i32;  //  78 px @ 256
    let inner_r = (0.117 * s) as f32;  //  30 px @ 256
    let outer_r = (0.195 * s) as f32;  //  50 px @ 256
    let body_y  = (0.453 * s) as i32;  // 116 px @ 256

    // Shackle arc — filled upper semicircle (dy ≤ 0)
    for py in 0..size {
        for px in 0..size {
            let dx = px as f32 - cx as f32;
            let dy = py as f32 - arc_cy as f32;
            let d  = (dx * dx + dy * dy).sqrt();
            if d >= inner_r && d <= outer_r && dy <= 0.0 {
                img.put_pixel(px, py, purple);
            }
        }
    }

    // Shackle posts (connect arc endpoints to top of lock body)
    let post_w = (outer_r - inner_r) as i32;
    let post_h = (body_y - arc_cy).max(0);
    let left_x  = (cx as f32 - outer_r) as i32;
    let right_x = (cx as f32 + inner_r) as i32;
    fill_rect(&mut img, left_x,  arc_cy, post_w, post_h, purple);
    fill_rect(&mut img, right_x, arc_cy, post_w, post_h, purple);

    // Lock body
    let bx = (0.188 * s) as i32; //  48 px @ 256
    let bw = (0.625 * s) as i32; // 160 px @ 256
    let bh = (0.406 * s) as i32; // 104 px @ 256
    let br = (0.063 * s) as i32; //  16 px @ 256  (corner radius)
    fill_rounded_rect(&mut img, bx, body_y, bw, bh, br, purple);

    // Keyhole — circle + pin, both in bg colour (cut out of the body)
    let kx = cx;
    let ky = body_y + (bh as f32 * 0.38) as i32;
    let kr = (0.063 * s) as i32;
    fill_circle(&mut img, kx, ky, kr, bg);
    fill_rect(  &mut img, kx - kr / 2, ky, kr, (kr as f32 * 1.8) as i32, bg);

    img
}

// ─── PNG / ICO helpers ────────────────────────────────────────────────────────

fn to_png_bytes(img: &RgbaImage) -> Vec<u8> {
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageOutputFormat::Png)
        .expect("PNG encode failed");
    buf
}

fn resize(img: &RgbaImage, size: u32) -> RgbaImage {
    image::imageops::resize(img, size, size, image::imageops::FilterType::Lanczos3)
}

// ─── Build entry point ────────────────────────────────────────────────────────

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // Generate the icon at full resolution.
    let icon256 = draw_icon(256);

    // Write PNG and export its path so main.rs can include_bytes!(env!(...)).
    // OUT_DIR is only visible inside build scripts; cargo:rustc-env bridges it.
    let icon_png = out.join("icon.png");
    std::fs::write(&icon_png, to_png_bytes(&icon256)).expect("write icon.png");
    println!("cargo:rustc-env=SECURENOTE_ICON_PNG={}", icon_png.display());

    // Build a multi-resolution ICO (16 → 256) for Windows.
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &sz in &[16u32, 24, 32, 48, 64, 128, 256] {
        let scaled = if sz == 256 { icon256.clone() } else { resize(&icon256, sz) };
        let entry  = ico::IconImage::from_rgba_data(sz, sz, scaled.into_raw());
        if let Ok(e) = ico::IconDirEntry::encode(&entry) {
            dir.add_entry(e);
        }
    }
    let ico_path = out.join("icon.ico");
    if let Ok(mut f) = std::fs::File::create(&ico_path) {
        let _ = dir.write(&mut f);
    }

    // Embed the ICO into the Windows .exe as a PE resource.
    // Uses CARGO_CFG_TARGET_OS (the build target) so cross-compiles are handled
    // correctly. Failure is non-fatal — the app still runs, just without the
    // embedded resource icon (runtime with_icon() still applies).
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        let ico_str = ico_path.to_string_lossy().into_owned();
        if let Err(e) = winresource::WindowsResource::new()
            .set_icon(&ico_str)
            .compile()
        {
            println!("cargo:warning=Icon embedding skipped (cross-compile?): {e}");
        }

        // Suppress the console window on Windows release builds.
        // Done here (not in main.rs) so panics and fatal_error() MessageBoxes
        // remain visible during debug builds.
        if std::env::var("PROFILE").unwrap_or_default() == "release" {
            println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:windows");
            println!("cargo:rustc-link-arg-bins=/ENTRY:mainCRTStartup");
        }
    }
}
