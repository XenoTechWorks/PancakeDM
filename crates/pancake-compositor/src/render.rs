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
    fn bar_moves_with_time_and_wraps() {
        assert_eq!(bar_x(0.0, 1000), 0);
        assert!(bar_x(1.0, 1000) > 400);
        assert!(bar_x(1.9, 1000) < 1000);
        assert_eq!(bar_x(2.0, 1000), bar_x(0.0, 1000));
    }
}
