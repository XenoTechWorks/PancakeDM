//! Software drawing into a frame buffer (XRGB8888). This is the simple
//! starting point; GPU rendering can replace it later behind the same idea:
//! draw a whole frame into the hidden buffer, then flip.

/// A frame to draw into: `stride` pixels per row, `width` x `height` visible.
pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    stride: usize,
    pub width: u32,
    pub height: u32,
}

impl<'a> Canvas<'a> {
    #[cfg_attr(not(feature = "zig-backend"), allow(dead_code))]
    pub fn new(pixels: &'a mut [u32], stride: u32, width: u32, height: u32) -> Self {
        assert!(stride >= width && pixels.len() >= (stride * height) as usize);
        Self { pixels, stride: stride as usize, width, height }
    }

    pub fn clear(&mut self, color: u32) {
        self.rect(0, 0, self.width, self.height, color);
    }

    /// Fill a rectangle, clipped to the canvas.
    pub fn rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        let x0 = x.min(self.width) as usize;
        let x1 = x.saturating_add(w).min(self.width) as usize;
        let y0 = y.min(self.height) as usize;
        let y1 = y.saturating_add(h).min(self.height) as usize;
        for row in y0..y1 {
            let start = row * self.stride;
            self.pixels[start + x0..start + x1].fill(color);
        }
    }
}

impl Canvas<'_> {
    /// Copy of the visible pixels, tightly packed (for screenshots).
    pub fn snapshot(&self) -> (u32, u32, Vec<u32>) {
        let mut out = Vec::with_capacity((self.width * self.height) as usize);
        for y in 0..self.height as usize {
            out.extend_from_slice(&self.pixels[y * self.stride..y * self.stride + self.width as usize]);
        }
        (self.width, self.height, out)
    }

    /// Draw a `w` x `h` image (premultiplied ARGB, tightly packed) with its
    /// top-left at (x, y), clipped to the canvas. `opaque` skips blending.
    pub fn blit(&mut self, x: i32, y: i32, w: u32, h: u32, src: &[u32], opaque: bool) {
        if src.len() < (w * h) as usize {
            return;
        }
        // Visible part of the image, in image coordinates.
        let sx0 = (-x).max(0) as u32;
        let sx1 = w.min((self.width as i32 - x).max(0) as u32);
        let sy0 = (-y).max(0) as u32;
        let sy1 = h.min((self.height as i32 - y).max(0) as u32);
        if sx0 >= sx1 || sy0 >= sy1 {
            return;
        }
        for sy in sy0..sy1 {
            let src_row = &src[(sy * w + sx0) as usize..(sy * w + sx1) as usize];
            let d = (y + sy as i32) as usize * self.stride + (x + sx0 as i32) as usize;
            let dst_row = &mut self.pixels[d..d + src_row.len()];
            if opaque {
                dst_row.copy_from_slice(src_row);
            } else {
                for (d, &s) in dst_row.iter_mut().zip(src_row) {
                    *d = blend(s, *d);
                }
            }
        }
    }
}

impl Canvas<'_> {
    /// Like `blit`, but pixels inside `clip` (x, y, w, h, radius) near its
    /// corners fade out along the rounded edge. Pixels outside the clip
    /// rect (e.g. an app's own drop shadow) are drawn as they are.
    pub fn blit_rounded(&mut self, x: i32, y: i32, w: u32, h: u32, src: &[u32], opaque: bool, clip: (i32, i32, i32, i32, i32)) {
        let (cx, cy, cw, ch, r) = clip;
        if r <= 0 || src.len() < (w * h) as usize {
            return self.blit(x, y, w, h, src, opaque);
        }
        let sx0 = (-x).max(0) as u32;
        let sx1 = w.min((self.width as i32 - x).max(0) as u32);
        let sy0 = (-y).max(0) as u32;
        let sy1 = h.min((self.height as i32 - y).max(0) as u32);
        if sx0 >= sx1 || sy0 >= sy1 {
            return;
        }
        let rf = r as f32;
        for sy in sy0..sy1 {
            let py = y + sy as i32;
            let src_row = &src[(sy * w + sx0) as usize..(sy * w + sx1) as usize];
            let d0 = py as usize * self.stride + (x + sx0 as i32) as usize;
            let corner_row = py >= cy && py < cy + ch && (py < cy + r || py >= cy + ch - r);
            if !corner_row {
                let dst = &mut self.pixels[d0..d0 + src_row.len()];
                if opaque {
                    dst.copy_from_slice(src_row);
                } else {
                    for (d, &s) in dst.iter_mut().zip(src_row) {
                        *d = blend(s, *d);
                    }
                }
                continue;
            }
            for (k, &s) in src_row.iter().enumerate() {
                let px = x + sx0 as i32 + k as i32;
                let i = d0 + k;
                let s = if opaque { s | 0xff00_0000 } else { s };
                let in_corner = px >= cx && px < cx + cw && (px < cx + r || px >= cx + cw - r);
                if !in_corner {
                    self.pixels[i] = if opaque { s } else { blend(s, self.pixels[i]) };
                    continue;
                }
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let ccx = fx.max(cx as f32 + rf).min((cx + cw) as f32 - rf);
                let ccy = fy.max(cy as f32 + rf).min((cy + ch) as f32 - rf);
                let d = ((fx - ccx).powi(2) + (fy - ccy).powi(2)).sqrt() - rf;
                let cover = (0.5 - d).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.pixels[i] = blend(if cover < 1.0 { scale(s, cover) } else { s }, self.pixels[i]);
                }
            }
        }
    }

    /// An anti-aliased rounded-rectangle outline, `t` pixels thick, drawn
    /// inside the rect (x, y, w, h).
    pub fn ring(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, t: i32, color: u32) {
        let (rf, tf) = (r.max(0) as f32, t as f32);
        let sdf = |px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, rad: f32| {
            let cx = px.max(x0 + rad).min(x1 - rad);
            let cy = py.max(y0 + rad).min(y1 - rad);
            let corner = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - rad;
            let edge = (x0 - px).max(px - x1).max(y0 - py).max(py - y1);
            if rad > 0.0 { corner.max(edge) } else { edge }
        };
        let (x0, y0, x1, y1) = (x as f32, y as f32, (x + w) as f32, (y + h) as f32);
        let band = (r.max(0) + t + 1) as i32;
        let paint = |canvas: &mut Self, px: i32, py: i32| {
            if px < 0 || py < 0 || px >= canvas.width as i32 || py >= canvas.height as i32 {
                return;
            }
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let outer = (0.5 - sdf(fx, fy, x0, y0, x1, y1, rf)).clamp(0.0, 1.0);
            let inner = (0.5 - sdf(fx, fy, x0 + tf, y0 + tf, x1 - tf, y1 - tf, (rf - tf).max(0.0))).clamp(0.0, 1.0);
            let cover = outer - inner;
            if cover > 0.0 {
                let i = py as usize * canvas.stride + px as usize;
                canvas.pixels[i] = blend(scale(color, cover), canvas.pixels[i]);
            }
        };
        // Only the border band: top and bottom strips, then the sides.
        for py in y..(y + band).min(y + h) {
            for px in x..x + w {
                paint(self, px, py);
            }
        }
        for py in (y + h - band).max(y + band)..y + h {
            for px in x..x + w {
                paint(self, px, py);
            }
        }
        for py in (y + band)..(y + h - band) {
            for px in (x..x + band).chain((x + w - band).max(x + band)..x + w) {
                paint(self, px, py);
            }
        }
    }
}

/// Scale a premultiplied pixel by `f` (0..1).
fn scale(p: u32, f: f32) -> u32 {
    let ch = |s: u32| ((((p >> s) & 0xff) as f32 * f).round() as u32) << s;
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// Premultiplied "over": src + dst * (1 - src alpha).
fn blend(src: u32, dst: u32) -> u32 {
    let a = src >> 24;
    if a == 255 {
        return src;
    }
    if a == 0 {
        return dst;
    }
    let inv = 255 - a;
    let ch = |shift: u32| {
        let s = (src >> shift) & 0xff;
        let d = (dst >> shift) & 0xff;
        ((s + (d * inv + 127) / 255).min(255)) << shift
    };
    0xff00_0000 | ch(16) | ch(8) | ch(0)
}

/// Write XRGB pixels as a PNG. Uses uncompressed deflate blocks, so it
/// needs no compression library; files are big but always valid.
pub fn write_png(path: &std::path::Path, w: u32, h: u32, px: &[u32]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((w * h * 3 + h) as usize);
    for y in 0..h as usize {
        raw.push(0); // filter: none
        for &p in &px[y * w as usize..(y + 1) * w as usize] {
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    // zlib stream made of stored blocks.
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(chunks.peek().is_none() as u8);
        let len = c.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    fn crc32(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &byte in data {
            c ^= byte as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, out)
}

/// The desktop behind the windows: wallpaper (or a color), plus with --anim
/// a bar sliding across (tearing would break it into offset pieces).
pub struct Scene {
    pub color: u32,
    pub anim: bool,
    pub started: std::time::Instant,
    wallpaper: Option<pancake_draw::images::Image>,
    fit: pancake_draw::images::Fit,
    /// The wallpaper laid out for one screen size, reused every frame.
    cache: Option<(u32, u32, Vec<u32>)>,
}

const BAR_WIDTH: u32 = 64;
const BAR_COLOR: u32 = 0xfff3e3;
/// Seconds for the bar to cross the screen.
const BAR_PERIOD: f64 = 2.0;

impl Scene {
    pub fn new(color: u32, anim: bool) -> Self {
        Self { color, anim, started: std::time::Instant::now(), wallpaper: None, fit: pancake_draw::images::Fit::Fill, cache: None }
    }

    /// Use these settings; the wallpaper is decoded once here.
    pub fn configure(&mut self, cfg: &pancake_config::Config, color_override: Option<u32>) {
        use pancake_config::WallpaperMode as M;
        use pancake_draw::images::Fit;
        self.color = color_override.unwrap_or(cfg.background);
        self.fit = match cfg.wallpaper_mode {
            M::Fill => Fit::Fill,
            M::Fit => Fit::Fit,
            M::Center => Fit::Center,
            M::Stretch => Fit::Stretch,
        };
        self.wallpaper = cfg.wallpaper_path().and_then(|p| {
            let img = pancake_draw::images::load(&p, None);
            if img.is_none() {
                eprintln!("pancake-compositor: can't load wallpaper {}", p.display());
            }
            img
        });
        self.cache = None;
    }

    pub fn draw(&mut self, c: &mut Canvas) {
        match &self.wallpaper {
            Some(img) => {
                let (w, h) = (c.width, c.height);
                if self.cache.as_ref().map_or(true, |(cw, ch, _)| (*cw, *ch) != (w, h)) {
                    self.cache = Some((w, h, pancake_draw::images::wallpaper(img, w, h, self.fit, self.color)));
                }
                let (_, _, px) = self.cache.as_ref().unwrap();
                c.blit(0, 0, w, h, px, true);
            }
            None => c.clear(self.color),
        }
        if self.anim {
            let x = bar_x(self.started.elapsed().as_secs_f64(), c.width);
            c.rect(x, 0, BAR_WIDTH, c.height, BAR_COLOR);
        }
    }
}

/// Bar position from elapsed time (not frame count), so its speed is the
/// same at any refresh rate and doesn't jump if a frame is late.
fn bar_x(seconds: f64, width: u32) -> u32 {
    let travel = (width + BAR_WIDTH) as f64;
    let pos = (seconds / BAR_PERIOD).fract() * travel;
    (pos as u32).saturating_sub(BAR_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_is_clipped_and_respects_stride() {
        // 4x3 visible, stride 6 (2 padding pixels per row).
        let mut px = vec![0u32; 6 * 3];
        let mut c = Canvas::new(&mut px, 6, 4, 3);
        c.rect(2, 1, 100, 100, 7);
        assert_eq!(&px[0..6], &[0, 0, 0, 0, 0, 0]);
        assert_eq!(&px[6..12], &[0, 0, 7, 7, 0, 0]); // padding untouched
        assert_eq!(&px[12..18], &[0, 0, 7, 7, 0, 0]);
    }

    #[test]
    fn clear_fills_only_visible_pixels() {
        let mut px = vec![0u32; 5 * 2];
        Canvas::new(&mut px, 5, 3, 2).clear(9);
        assert_eq!(px, vec![9, 9, 9, 0, 0, 9, 9, 9, 0, 0]);
    }

    #[test]
    fn blit_clips_at_every_edge() {
        let mut px = vec![0u32; 4 * 4];
        let img = vec![5u32; 3 * 3];
        let mut c = Canvas::new(&mut px, 4, 4, 4);
        c.blit(-1, -1, 3, 3, &img, true); // top-left, partly off screen
        c.blit(3, 3, 3, 3, &img, true); // bottom-right, partly off screen
        c.blit(10, 10, 3, 3, &img, true); // fully off screen: no panic
        assert_eq!(px, vec![
            5, 5, 0, 0,
            5, 5, 0, 0,
            0, 0, 0, 0,
            0, 0, 0, 5,
        ]);
    }

    #[test]
    fn rounded_blit_cuts_corners_only() {
        let mut px = vec![0u32; 20 * 20];
        let img = vec![0xffff_ffffu32; 20 * 20];
        Canvas::new(&mut px, 20, 20, 20).blit_rounded(0, 0, 20, 20, &img, true, (0, 0, 20, 20, 6));
        assert_eq!(px[0], 0); // corner cut away
        assert_eq!(px[10 * 20 + 10], 0xffff_ffff); // middle untouched
        assert_eq!(px[10], 0xffff_ffff); // top edge middle untouched
    }

    #[test]
    fn ring_draws_only_the_outline() {
        let mut px = vec![0u32; 30 * 30];
        Canvas::new(&mut px, 30, 30, 30).ring(0, 0, 30, 30, 6, 2, 0xffff_0000);
        assert!(px[15 * 30] >> 24 > 200); // left edge
        assert_eq!(px[15 * 30 + 15], 0); // inside stays clear
    }

    #[test]
    fn blend_is_premultiplied_over() {
        assert_eq!(blend(0xff11_2233, 0xffff_ffff), 0xff11_2233); // opaque wins
        assert_eq!(blend(0x0000_0000, 0xff44_5566), 0xff44_5566); // clear keeps dst
        // 50% black over white: about half grey.
        let g = blend(0x8000_0000, 0xffff_ffff) & 0xff;
        assert!((126..=128).contains(&g), "{g}");
    }

    #[test]
    fn png_has_valid_structure() {
        let dir = std::env::temp_dir().join(format!("pancake-png-test-{}", std::process::id()));
        write_png(&dir, 2, 2, &[0xff0000, 0x00ff00, 0x0000ff, 0xffffff]).unwrap();
        let data = std::fs::read(&dir).unwrap();
        let _ = std::fs::remove_file(&dir);
        assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&data[12..16], b"IHDR");
        assert_eq!(&data[data.len() - 8..data.len() - 4], b"IEND");
    }

    #[test]
    fn bar_moves_with_time_and_wraps() {
        assert_eq!(bar_x(0.0, 1000), 0);
        assert!(bar_x(1.0, 1000) > 400);
        assert!(bar_x(1.9, 1000) < 1000);
        assert_eq!(bar_x(2.0, 1000), bar_x(0.0, 1000));
    }
}
