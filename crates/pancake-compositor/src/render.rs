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

/// What --fill shows: a background color, plus with --anim a bar sliding
/// across. Tearing would show up as the bar breaking into offset pieces.
pub struct Scene {
    pub color: u32,
    pub anim: bool,
    pub started: std::time::Instant,
}

const BAR_WIDTH: u32 = 64;
const BAR_COLOR: u32 = 0xfff3e3;
/// Seconds for the bar to cross the screen.
const BAR_PERIOD: f64 = 2.0;

impl Scene {
    pub fn draw(&self, c: &mut Canvas) {
        c.clear(self.color);
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
