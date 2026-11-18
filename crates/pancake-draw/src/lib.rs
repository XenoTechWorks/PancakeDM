//! Shared pieces for PancakeDM's shell programs (panel, launcher, ...):
//! anti-aliased rounded rectangles and text into a premultiplied ARGB8888
//! buffer, the color palette, and small Wayland/system helpers.
//!
//! Everything is drawn by hand; there's no toolkit.

pub mod accent;
pub mod apps;
pub mod icons;
pub mod images;

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
    /// Readable text on a 0xRRGGBB background: dark on light, light on dark.
    pub fn text_on(rgb: u32) -> u32 {
        let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
        if r * 299 + g * 587 + b * 114 > 140_000 { ON_GOLD } else { TEXT }
    }

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

/// A buffer to draw into. Coordinates and sizes are logical: with
/// `scale` 2, a 10x10 rectangle covers 20x20 pixels and text is twice as
/// tall, so HiDPI rendering needs no changes in drawing code. `w` and `h`
/// are the logical size.
pub struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub w: i32,
    pub h: i32,
    pub scale: f32,
}

/// Premultiply a straight-alpha 0xAARRGGBB color, scaled by `cover` (0..1).
fn premul(argb: u32, cover: f32) -> u32 {
    let a = ((argb >> 24) as f32 * cover).round() as u32;
    let ch = |s: u32| ((((argb >> s) & 0xff) * a + 127) / 255) << s;
    (a << 24) | ch(16) | ch(8) | ch(0)
}

/// Scale a premultiplied pixel's alpha (and so its color) by `a`.
fn fade(p: u32, a: f32) -> u32 {
    let ch = |s: u32| ((((p >> s) & 0xff) as f32 * a) as u32) << s;
    ch(24) | ch(16) | ch(8) | ch(0)
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

impl<'a> Canvas<'a> {
    /// A canvas over `px` (physical pixels, `lw*scale` x `lh*scale`).
    pub fn new(px: &'a mut [u32], lw: i32, lh: i32, scale: f32) -> Self {
        Self { px, w: lw, h: lh, scale }
    }

    fn pw(&self) -> i32 {
        (self.w as f32 * self.scale).round() as i32
    }

    fn ph(&self) -> i32 {
        (self.h as f32 * self.scale).round() as i32
    }

    pub fn clear(&mut self) {
        self.px.fill(0);
    }

    /// Anti-aliased rounded rectangle, straight-alpha color.
    pub fn rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: u32) {
        let s = self.scale;
        let (x, y, w, h, r) = (x * s, y * s, w * s, h * s, r * s);
        let (pw, ph) = (self.pw(), self.ph());
        let r = r.min(w / 2.0).min(h / 2.0);
        let (x0, y0) = (x.floor().max(0.0) as i32, y.floor().max(0.0) as i32);
        let (x1, y1) = (((x + w).ceil() as i32).min(pw), ((y + h).ceil() as i32).min(ph));
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                // Signed distance to the rounded rectangle (negative inside).
                let (hx, hy) = (w / 2.0, h / 2.0);
                let qx = (fx - (x + hx)).abs() - (hx - r);
                let qy = (fy - (y + hy)).abs() - (hy - r);
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let d = outside + qx.max(qy).min(0.0) - r;
                let cover = (0.5 - d).clamp(0.0, 1.0);
                if cover > 0.0 {
                    let i = (py * pw + px) as usize;
                    self.px[i] = over(premul(color, cover), self.px[i]);
                }
            }
        }
    }

    /// Draw a premultiplied image with its top-left at logical (x, y).
    /// The image is drawn pixel for pixel, so load it at `size * scale`.
    pub fn image(&mut self, img: &images::Image, x: i32, y: i32) {
        self.image_alpha(img, x as f32, y as f32, 1.0);
    }

    /// Like `image`, faded by `alpha` (0..1).
    pub fn image_alpha(&mut self, img: &images::Image, x: f32, y: f32, alpha: f32) {
        let (pw, ph) = (self.pw(), self.ph());
        let (x, y) = ((x * self.scale).round() as i32, (y * self.scale).round() as i32);
        for row in 0..img.h as i32 {
            let dy = y + row;
            if dy < 0 || dy >= ph {
                continue;
            }
            for col in 0..img.w as i32 {
                let dx = x + col;
                if dx < 0 || dx >= pw {
                    continue;
                }
                let src = img.px[(row * img.w as i32 + col) as usize];
                if src >> 24 != 0 {
                    let src = if alpha < 1.0 { fade(src, alpha) } else { src };
                    let i = (dy * pw + dx) as usize;
                    self.px[i] = over(src, self.px[i]);
                }
            }
        }
    }

    /// Fade the whole canvas by `alpha` (for fade-in/out animations).
    pub fn fade_all(&mut self, alpha: f32) {
        if alpha >= 1.0 {
            return;
        }
        for p in self.px.iter_mut() {
            *p = fade(*p, alpha);
        }
    }

    /// An anti-aliased arc (ring segment) centered on (cx, cy) with radius
    /// `r` (to the middle of the stroke), `t` thick. Angles in degrees,
    /// 0 = up, clockwise; `sweep` 360 draws a full ring.
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, t: f32, start: f32, sweep: f32, color: u32) {
        let s = self.scale;
        let (cx, cy, r, t) = (cx * s, cy * s, r * s, t * s);
        let (pw, ph) = (self.pw(), self.ph());
        let (outer, inner) = (r + t / 2.0, r - t / 2.0);
        let (x0, y0) = (((cx - outer).floor() as i32).max(0), ((cy - outer).floor() as i32).max(0));
        let (x1, y1) = (((cx + outer).ceil() as i32).min(pw), ((cy + outer).ceil() as i32).min(ph));
        let sweep = sweep.clamp(0.0, 360.0);
        // Round caps: centers of the two ends.
        let end = |deg: f32| {
            let a = deg.to_radians();
            (cx + r * a.sin(), cy - r * a.cos())
        };
        let (c0, c1) = (end(start), end(start + sweep));
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let (dx, dy) = (fx - cx, fy - cy);
                let d = (dx * dx + dy * dy).sqrt();
                let radial = (0.5 - (d - outer)).clamp(0.0, 1.0) * (0.5 - (inner - d)).clamp(0.0, 1.0);
                let mut cover = 0.0;
                if radial > 0.0 {
                    let ang = (dx.atan2(-dy).to_degrees() + 360.0) % 360.0;
                    let rel = (ang - start % 360.0 + 720.0) % 360.0;
                    if sweep >= 360.0 || rel <= sweep {
                        cover = radial;
                    }
                }
                if sweep < 360.0 {
                    for (ex, ey) in [c0, c1] {
                        let de = ((fx - ex).powi(2) + (fy - ey).powi(2)).sqrt();
                        cover = f32::max(cover, (0.5 - (de - t / 2.0)).clamp(0.0, 1.0));
                    }
                }
                if cover > 0.0 {
                    let i = (py * pw + px) as usize;
                    self.px[i] = over(premul(color, cover), self.px[i]);
                }
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: u32) {
        self.rounded_rect(cx - r, cy - r, r * 2.0, r * 2.0, r, color);
    }

    /// Draw text with its baseline at `y`; returns the (logical) width drawn.
    pub fn text(&mut self, fonts: &mut Fonts, text: &str, x: f32, y: f32, px: f32, bold: bool, color: u32) -> f32 {
        let s = self.scale;
        let (pw, ph) = (self.pw(), self.ph());
        let (x, y, px) = (x * s, y * s, px * s);
        let mut pen = x;
        for c in text.chars() {
            let (m, bitmap) = fonts.glyph(c, px, bold).clone();
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (y - m.height as f32 - m.ymin as f32).round() as i32;
            for row in 0..m.height as i32 {
                for col in 0..m.width as i32 {
                    let (dx, dy) = (gx + col, gy + row);
                    if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                        continue;
                    }
                    let cover = bitmap[(row * m.width as i32 + col) as usize] as f32 / 255.0;
                    if cover > 0.0 {
                        let i = (dy * pw + dx) as usize;
                        self.px[i] = over(premul(color, cover), self.px[i]);
                    }
                }
            }
            pen += m.advance_width;
        }
        (pen - x) / s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_two_doubles_everything() {
        let mut px = vec![0u32; 20 * 20];
        let mut c = Canvas::new(&mut px, 10, 10, 2.0);
        c.rounded_rect(0.0, 0.0, 5.0, 5.0, 0.0, 0xffff_ffff);
        assert_eq!(px[9 * 20 + 9], 0xffff_ffff); // inside the 10x10 physical square
        assert_eq!(px[11 * 20 + 11], 0); // outside it
    }

    #[test]
    fn square_rects_are_fully_opaque_inside() {
        let mut px = vec![0u32; 10 * 10];
        Canvas::new(&mut px, 10, 10, 1.0).rounded_rect(0.0, 0.0, 10.0, 10.0, 0.0, 0xff00_ff00);
        assert!(px.iter().all(|&p| p == 0xff00_ff00));
    }

    #[test]
    fn arc_covers_only_its_sweep() {
        let mut px = vec![0u32; 40 * 40];
        let mut c = Canvas::new(&mut px, 40, 40, 1.0);
        c.arc(20.0, 20.0, 15.0, 4.0, 0.0, 90.0, 0xffff_ffff); // top to right
        assert!(px[20 * 40 + 35] >> 24 > 200); // right side (90 deg)
        assert_eq!(px[35 * 40 + 20], 0); // bottom (180 deg): not drawn
        assert_eq!(px[20 * 40 + 20], 0); // center: not drawn
    }

    #[test]
    fn premultiplied_colors() {
        assert_eq!(premul(0xff11_2233, 1.0), 0xff11_2233);
        assert_eq!(premul(0xffff_ffff, 0.0), 0);
        let half = premul(0x80ff_ffff, 1.0);
        assert_eq!(half >> 24, 0x80);
        assert_eq!(half & 0xff, 0x80); // channel scaled by alpha
    }

    #[test]
    fn circles_with_fractional_sizes_dont_panic() {
        let mut px = vec![0u32; 64 * 64];
        let mut c = Canvas::new(&mut px, 64, 64, 1.0);
        for i in 0..200 {
            let r = 2.0 + i as f32 * 0.137;
            c.circle(32.0, 32.0, r, 0xffffffff);
        }
    }

    #[test]
    fn rounded_rect_has_soft_corners_and_solid_middle() {
        let mut px = vec![0u32; 20 * 20];
        let mut c = Canvas::new(&mut px, 20, 20, 1.0);
        c.rounded_rect(0.0, 0.0, 20.0, 20.0, 8.0, 0xff00_0000);
        assert_eq!(px[10 * 20 + 10] >> 24, 255); // middle solid
        assert_eq!(px[0] >> 24, 0); // corner pixel outside the curve
    }
}
