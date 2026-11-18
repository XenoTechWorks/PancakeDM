//! Accent color from the wallpaper (in the spirit of Material You): find the
//! most prominent lively hue, then tune it so it reads well on dark glass.
//! Results are cached per wallpaper file, so programs don't redo the work.

use crate::images::Image;
use std::path::{Path, PathBuf};

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |f: f32| (((f + m) * 255.0).round() as u32).min(255);
    (to(r) << 16) | (to(g) << 8) | to(b)
}

/// The accent for an image, as 0xRRGGBB; `fallback` if it's all grey.
pub fn from_image(img: &Image, fallback: u32) -> u32 {
    // Hue histogram over a sample of pixels, weighted toward lively ones.
    const BINS: usize = 36;
    let mut weight = [0f32; BINS];
    let mut sums = [(0f32, 0f32, 0f32); BINS];
    let step = ((img.px.len() / 40_000).max(1)) as usize;
    for &p in img.px.iter().step_by(step) {
        let a = (p >> 24) as f32 / 255.0;
        if a < 0.5 {
            continue;
        }
        let (r, g, b) = (((p >> 16) & 0xff) as f32 / 255.0, ((p >> 8) & 0xff) as f32 / 255.0, (p & 0xff) as f32 / 255.0);
        let (h, s, v) = rgb_to_hsv(r, g, b);
        if s < 0.25 || v < 0.25 {
            continue; // greys, near-black: not accent material
        }
        let bin = ((h / 360.0) * BINS as f32) as usize % BINS;
        let w = s * v;
        weight[bin] += w;
        sums[bin].0 += h * w;
        sums[bin].1 += s * w;
        sums[bin].2 += v * w;
    }
    // Smooth over neighbouring bins so a hue split across two still wins.
    let (best, total) = (0..BINS)
        .map(|i| (i, weight[i] + 0.5 * (weight[(i + BINS - 1) % BINS] + weight[(i + 1) % BINS])))
        .fold((0, 0.0), |acc, (i, w)| if w > acc.1 { (i, w) } else { acc });
    if total <= 0.0 || weight[best] <= 0.0 {
        return fallback;
    }
    let w = weight[best];
    let (h, s, v) = (sums[best].0 / w, sums[best].1 / w, sums[best].2 / w);
    // Readable on dark glass: fairly saturated, fairly bright, not neon.
    hsv_to_rgb(h, s.clamp(0.45, 0.75), v.clamp(0.75, 0.95))
}

fn cache_path(wallpaper: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(wallpaper).ok()?;
    let stamp = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let key = wallpaper.to_string_lossy().bytes().fold(5381u64, |h, b| h.wrapping_mul(33) ^ b as u64);
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("pancake").join(format!("accent-{key:016x}-{stamp}")))
}

/// The accent to use: the configured color, or (with `accent = auto`) the
/// wallpaper's, cached on disk.
pub fn effective(cfg: &pancake_config::Config) -> u32 {
    if !cfg.accent_auto {
        return cfg.accent;
    }
    let Some(wall) = cfg.wallpaper_path() else { return cfg.accent };
    let cache = cache_path(&wall);
    if let Some(hex) = cache.as_ref().and_then(|c| std::fs::read_to_string(c).ok()) {
        if let Ok(v) = u32::from_str_radix(hex.trim(), 16) {
            return v;
        }
    }
    let Some(img) = crate::images::load(&wall, None) else { return cfg.accent };
    let accent = from_image(&img, cfg.accent);
    if let Some(c) = cache {
        if let Some(dir) = c.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(c, format!("{accent:06x}"));
    }
    accent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_dominant_lively_hue() {
        // Mostly grey with a big red patch and a small blue one: red wins.
        let mut px = vec![0xff80_8080u32; 1000];
        px[..300].fill(0xffd0_3030);
        px[300..350].fill(0xff30_30d0);
        let a = from_image(&Image { w: 1000, h: 1, px }, 0x123456);
        let (r, g, b) = ((a >> 16) & 0xff, (a >> 8) & 0xff, a & 0xff);
        assert!(r > g + 60 && r > b + 60, "{a:06x}");
    }

    #[test]
    fn grey_images_fall_back() {
        let img = Image { w: 10, h: 1, px: vec![0xff77_7777; 10] };
        assert_eq!(from_image(&img, 0x123456), 0x123456);
    }

    #[test]
    fn hsv_round_trip() {
        let (h, s, v) = rgb_to_hsv(0.957, 0.635, 0.349); // f4a259
        assert_eq!(hsv_to_rgb(h, s, v), 0xf4a259);
    }
}
