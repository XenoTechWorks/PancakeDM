//! Shared pieces for PancakeDM's shell programs (panel, launcher, ...):
//! anti-aliased rounded rectangles and text into a premultiplied ARGB8888
//! buffer, the color palette, and small Wayland/system helpers.
//!
//! Everything is drawn by hand; there's no toolkit.

use fontdue::{Font, FontSettings};
use std::collections::HashMap;

pub struct Fonts {
    pub regular: Font,
    pub bold: Font,
    cache: HashMap<(char, u32, bool), (fontdue::Metrics, Vec<u8>)>,
}

/// Find a font file through fontconfig, falling back to common paths.
fn find_font(pattern: &str, fallbacks: &[&str]) -> Option<Vec<u8>> {
    let from_fc = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .filter(|p| !p.is_empty());
    from_fc
        .into_iter()
        .chain(fallbacks.iter().map(|s| s.to_string()))
        .find_map(|p| std::fs::read(p).ok())
}

impl Fonts {
    pub fn load() -> Result<Self, String> {
        let load = |pattern: &str, fallbacks: &[&str]| -> Result<Font, String> {
            let bytes = find_font(pattern, fallbacks).ok_or(format!("no font found for {pattern}"))?;
            Font::from_bytes(bytes, FontSettings::default()).map_err(|e| e.to_string())
        };
        Ok(Self {
            regular: load("sans-serif", &["/usr/share/fonts/noto/NotoSans-Regular.ttf", "/usr/share/fonts/TTF/DejaVuSans.ttf"])?,
            bold: load("sans-serif:bold", &["/usr/share/fonts/noto/NotoSans-Bold.ttf", "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"])?,
            cache: HashMap::new(),
        })
    }

    fn glyph(&mut self, c: char, px: f32, bold: bool) -> &(fontdue::Metrics, Vec<u8>) {
        let key = (c, px.to_bits(), bold);
        let font = if bold { &self.bold } else { &self.regular };
        self.cache.entry(key).or_insert_with(|| font.rasterize(c, px))
    }

    pub fn width(&mut self, text: &str, px: f32, bold: bool) -> f32 {
        text.chars().map(|c| self.glyph(c, px, bold).0.advance_width).sum()
    }

    /// Cut `text` to fit `max` pixels, ending with an ellipsis if cut.
    pub fn fit(&mut self, text: &str, px: f32, bold: bool, max: f32) -> String {
        if self.width(text, px, bold) <= max {
            return text.to_string();
        }
        let ell = self.width("…", px, bold);
        let mut out = String::new();
        let mut w = 0.0;
        for c in text.chars() {
            let cw = self.glyph(c, px, bold).0.advance_width;
            if w + cw + ell > max {
                break;
            }
            w += cw;
            out.push(c);
        }
        out.push('…');
        out
    }
}

/// The PancakeDM palette (straight alpha 0xAARRGGBB).
pub mod color {
    pub const GLASS: u32 = 0xe61f1a17; // warm near-black glass
    pub const GLASS_EDGE: u32 = 0x22ffffff;
    pub const TEXT: u32 = 0xfff6eee3;
    pub const TEXT_DIM: u32 = 0xb3f6eee3;
    pub const TEXT_FAINT: u32 = 0x66f6eee3;
    pub const GOLD: u32 = 0xfff4a259;
    pub const GOLD_DARK: u32 = 0xffc9792f;
    pub const ON_GOLD: u32 = 0xff2a1d12;
    pub const SUBTLE: u32 = 0x1affffff;
    pub const SUBTLE_HOVER: u32 = 0x2effffff;
    /// Warm, distinct colors for things like app badges.
    pub const ACCENTS: [u32; 8] =
        [0xffbc4b51, 0xff5b8e7d, 0xff8cb369, 0xfff4a259, 0xff3d5a80, 0xff8e5b7d, 0xff2a9d8f, 0xffc9792f];
}

/// An unlinked, mapped file in XDG_RUNTIME_DIR for wl_shm buffers.
pub fn shm_file(size: u64, tag: &str) -> Result<(std::fs::File, memmap2::MmapMut), String> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR isn't set")?;
    let path = std::path::Path::new(&dir).join(format!("pancake-{tag}-{}", std::process::id()));
    let file = std::fs::File::options().read(true).write(true).create(true).truncate(true).open(&path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);
    file.set_len(size).map_err(|e| e.to_string())?;
    let mem = unsafe { memmap2::MmapMut::map_mut(&file) }.map_err(|e| e.to_string())?;
    Ok((file, mem))
}

/// View a mapped buffer as pixels.
pub fn pixels(mem: &mut memmap2::MmapMut) -> &mut [u32] {
    let len = mem.len() / 4;
    // Safety: mmap'd memory is page-aligned, so it's aligned for u32.
    unsafe { std::slice::from_raw_parts_mut(mem.as_mut_ptr() as *mut u32, len) }
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

extern "C" {
    fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
}

/// Wait up to `timeout_ms` for `fd` to become readable.
pub fn wait_readable(fd: i32, timeout_ms: i32) -> bool {
    let mut p = PollFd { fd, events: 1, revents: 0 };
    unsafe { poll(&mut p, 1, timeout_ms) > 0 && p.revents != 0 }
}

pub struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub w: i32,
    pub h: i32,
}

/// Premultiply a straight-alpha 0xAARRGGBB color, scaled by `cover` (0..1).
fn premul(argb: u32, cover: f32) -> u32 {
    let a = ((argb >> 24) as f32 * cover).round() as u32;
    let ch = |s: u32| ((((argb >> s) & 0xff) * a + 127) / 255) << s;
    (a << 24) | ch(16) | ch(8) | ch(0)
}

/// Premultiplied "over".
fn over(src: u32, dst: u32) -> u32 {
    let a = src >> 24;
    if a == 255 {
        return src;
    }
    let inv = 255 - a;
    let ch = |s: u32| (((src >> s) & 0xff) + (((dst >> s) & 0xff) * inv + 127) / 255).min(255) << s;
    ch(24) | ch(16) | ch(8) | ch(0)
}

impl Canvas<'_> {
    pub fn clear(&mut self) {
        self.px.fill(0);
    }

    /// Anti-aliased rounded rectangle, straight-alpha color.
    pub fn rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: u32) {
        let r = r.min(w / 2.0).min(h / 2.0);
        let (x0, y0) = (x.floor().max(0.0) as i32, y.floor().max(0.0) as i32);
        let (x1, y1) = (((x + w).ceil() as i32).min(self.w), ((y + h).ceil() as i32).min(self.h));
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                // Distance outside the rounded shape (negative inside).
                let cx = fx.clamp(x + r, x + w - r);
                let cy = fy.clamp(y + r, y + h - r);
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() - r;
                let edge_x = (fx - x).min(x + w - fx);
                let edge_y = (fy - y).min(y + h - fy);
                let cover = (0.5 - d).clamp(0.0, 1.0) * (edge_x + 0.5).clamp(0.0, 1.0) * (edge_y + 0.5).clamp(0.0, 1.0);
                if cover > 0.0 {
                    let i = (py * self.w + px) as usize;
                    self.px[i] = over(premul(color, cover), self.px[i]);
                }
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: u32) {
        self.rounded_rect(cx - r, cy - r, r * 2.0, r * 2.0, r, color);
    }

    /// Draw text with its baseline at `y`; returns the width drawn.
    pub fn text(&mut self, fonts: &mut Fonts, text: &str, x: f32, y: f32, px: f32, bold: bool, color: u32) -> f32 {
        let mut pen = x;
        for c in text.chars() {
            let (m, bitmap) = fonts.glyph(c, px, bold).clone();
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (y - m.height as f32 - m.ymin as f32).round() as i32;
            for row in 0..m.height as i32 {
                for col in 0..m.width as i32 {
                    let (dx, dy) = (gx + col, gy + row);
                    if dx < 0 || dy < 0 || dx >= self.w || dy >= self.h {
                        continue;
                    }
                    let cover = bitmap[(row * m.width as i32 + col) as usize] as f32 / 255.0;
                    if cover > 0.0 {
                        let i = (dy * self.w + dx) as usize;
                        self.px[i] = over(premul(color, cover), self.px[i]);
                    }
                }
            }
            pen += m.advance_width;
        }
        pen - x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_colors() {
        assert_eq!(premul(0xff11_2233, 1.0), 0xff11_2233);
        assert_eq!(premul(0xffff_ffff, 0.0), 0);
        let half = premul(0x80ff_ffff, 1.0);
        assert_eq!(half >> 24, 0x80);
        assert_eq!(half & 0xff, 0x80); // channel scaled by alpha
    }

    #[test]
    fn rounded_rect_has_soft_corners_and_solid_middle() {
        let mut px = vec![0u32; 20 * 20];
        let mut c = Canvas { px: &mut px, w: 20, h: 20 };
        c.rounded_rect(0.0, 0.0, 20.0, 20.0, 8.0, 0xff00_0000);
        assert_eq!(px[10 * 20 + 10] >> 24, 255); // middle solid
        assert_eq!(px[0] >> 24, 0); // corner pixel outside the curve
    }
}
