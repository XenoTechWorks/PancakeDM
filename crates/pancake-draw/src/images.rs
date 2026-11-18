//! Loading images (PNG, JPEG, SVG) into premultiplied ARGB pixels, and
//! scaling them for wallpapers.

use std::path::Path;

/// Premultiplied ARGB8888 pixels, tightly packed.
#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

fn from_rgba(w: u32, h: u32, rgba: &[u8], premultiplied: bool) -> Image {
    let px = rgba
        .chunks_exact(4)
        .map(|p| {
            let a = p[3] as u32;
            let ch = |c: u8| if premultiplied { c as u32 } else { (c as u32 * a + 127) / 255 };
            (a << 24) | (ch(p[0]) << 16) | (ch(p[1]) << 8) | ch(p[2])
        })
        .collect();
    Image { w, h, px }
}

/// Load an image. `size` scales it to exactly that size (icons); SVGs are
/// rendered at that size, so they stay sharp. Without a size, raster images
/// keep theirs and SVGs render at their own nominal size.
pub fn load(path: &Path, size: Option<(u32, u32)>) -> Option<Image> {
    let data = std::fs::read(path).ok()?;
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        return load_svg(&data, size);
    }
    let img = image::load_from_memory(&data).ok()?.to_rgba8();
    let img = match size {
        Some((w, h)) if (w, h) != img.dimensions() => image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle),
        _ => img,
    };
    Some(from_rgba(img.width(), img.height(), img.as_raw(), false))
}

/// Render SVG data (e.g. a built-in icon).
pub fn load_svg_bytes(data: &[u8], size: Option<(u32, u32)>) -> Option<Image> {
    load_svg(data, size)
}

fn load_svg(data: &[u8], size: Option<(u32, u32)>) -> Option<Image> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let nat = tree.size();
    let (w, h) = size.unwrap_or((nat.width().ceil() as u32, nat.height().ceil() as u32));
    let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
    let t = tiny_skia::Transform::from_scale(w as f32 / nat.width(), h as f32 / nat.height());
    resvg::render(&tree, t, &mut pixmap.as_mut());
    Some(from_rgba(w, h, pixmap.data(), true))
}

/// How a wallpaper covers the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Fill the screen, cropping what sticks out (default).
    Fill,
    /// Show all of it, with the background color around it.
    Fit,
    /// Native size, centered.
    Center,
    /// Fill the screen, distorting the aspect ratio.
    Stretch,
}

/// Lay `img` out on a `w` x `h` screen over `background` (0xRRGGBB).
/// Returns opaque pixels ready to copy every frame.
pub fn wallpaper(img: &Image, w: u32, h: u32, fit: Fit, background: u32) -> Vec<u32> {
    let mut out = vec![0xff00_0000 | background; (w * h) as usize];
    let (iw, ih) = (img.w as f64, img.h as f64);
    let (sw, sh) = match fit {
        Fit::Fill => {
            let s = (w as f64 / iw).max(h as f64 / ih);
            ((iw * s).round() as u32, (ih * s).round() as u32)
        }
        Fit::Fit => {
            let s = (w as f64 / iw).min(h as f64 / ih);
            ((iw * s).round() as u32, (ih * s).round() as u32)
        }
        Fit::Center => (img.w, img.h),
        Fit::Stretch => (w, h),
    };
    let scaled = if (sw, sh) == (img.w, img.h) {
        img.clone()
    } else {
        // Back through image's resampler (Triangle is smooth and quick).
        let rgba: Vec<u8> = img.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]).collect();
        let buf = image::RgbaImage::from_raw(img.w, img.h, rgba).unwrap();
        let r = image::imageops::resize(&buf, sw.max(1), sh.max(1), image::imageops::FilterType::Triangle);
        from_rgba(r.width(), r.height(), r.as_raw(), true)
    };
    // Center (cropping or letterboxing as needed).
    let ox = (w as i64 - scaled.w as i64) / 2;
    let oy = (h as i64 - scaled.h as i64) / 2;
    for y in 0..h as i64 {
        let sy = y - oy;
        if sy < 0 || sy >= scaled.h as i64 {
            continue;
        }
        for x in 0..w as i64 {
            let sx = x - ox;
            if sx < 0 || sx >= scaled.w as i64 {
                continue;
            }
            let p = scaled.px[(sy * scaled.w as i64 + sx) as usize];
            out[(y * w as i64 + x) as usize] = 0xff00_0000 | p; // wallpapers are opaque
        }
    }
    out
}

/// A soft, darkened backdrop from an image (album art behind a card):
/// cover-fit into `w` x `h`, blurred by shrinking a lot and scaling back
/// up smoothly, then dimmed by `dim` (0..1, how much to darken).
pub fn blurred_backdrop(img: &Image, w: u32, h: u32, dim: f32) -> Image {
    let tiny = (w / 14).max(2);
    let tiny_h = (h / 14).max(2);
    let small = wallpaper(img, tiny, tiny_h, Fit::Fill, 0);
    let rgba: Vec<u8> = small.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
    let buf = image::RgbaImage::from_raw(tiny, tiny_h, rgba).unwrap();
    let big = image::imageops::resize(&buf, w.max(1), h.max(1), image::imageops::FilterType::CatmullRom);
    let keep = 1.0 - dim.clamp(0.0, 1.0);
    let px = big
        .pixels()
        .map(|p| {
            let c = |v: u8| ((v as f32 * keep) as u32).min(255);
            0xff00_0000 | (c(p[0]) << 16) | (c(p[1]) << 8) | c(p[2])
        })
        .collect();
    Image { w, h, px }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: u32) -> Image {
        Image { w, h, px: vec![c; (w * h) as usize] }
    }

    #[test]
    fn fit_letterboxes_with_background() {
        // A wide image on a square screen: bars top and bottom.
        let out = wallpaper(&solid(20, 10, 0xffff0000), 10, 10, Fit::Fit, 0x0000ff);
        assert_eq!(out[0], 0xff0000ff); // top bar
        assert_eq!(out[5 * 10 + 5], 0xffff0000); // middle is image
    }

    #[test]
    fn fill_covers_everything() {
        let out = wallpaper(&solid(20, 10, 0xffff0000), 10, 10, Fit::Fill, 0x0000ff);
        assert!(out.iter().all(|&p| p == 0xffff0000));
    }

    #[test]
    fn center_keeps_native_size() {
        let out = wallpaper(&solid(2, 2, 0xffff0000), 6, 6, Fit::Center, 0x000000);
        assert_eq!(out.iter().filter(|&&p| p == 0xffff0000).count(), 4);
    }

    #[test]
    fn straight_alpha_is_premultiplied_on_load() {
        let img = from_rgba(1, 1, &[255, 255, 255, 128], false);
        assert_eq!(img.px[0], 0x8080_8080);
    }
}

#[cfg(test)]
mod decode_check {
    /// Manual check: PANCAKE_TEST_IMAGE=/path/to.jpg cargo test -p pancake-draw decode_check -- --ignored --nocapture
    #[test]
    #[ignore]
    fn count_colors() {
        let Some(p) = std::env::var_os("PANCAKE_TEST_IMAGE") else { return };
        let img = super::load(std::path::Path::new(&p), None).unwrap();
        let uniq: std::collections::HashSet<u32> = img.px.iter().copied().collect();
        println!("decoded {}x{}, {} colors", img.w, img.h, uniq.len());
        let wp = super::wallpaper(&img, 1274, 1388, super::Fit::Fill, 0);
        let uniq2: std::collections::HashSet<u32> = wp.iter().copied().collect();
        println!("wallpaper 1274x1388, {} colors", uniq2.len());
        let raw = image::open(&p).unwrap().to_rgb8();
        let uniq3: std::collections::HashSet<[u8; 3]> = raw.pixels().map(|p| p.0).collect();
        println!("image crate direct: {} colors", uniq3.len());
    }
}
