//! Tiny 2D drawing for the panel: anti-aliased rounded rectangles and
//! text, into a premultiplied ARGB8888 buffer.

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
