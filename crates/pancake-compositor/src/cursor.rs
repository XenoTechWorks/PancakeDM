//! Cursor image and pointer position.
//!
//! The image is uploaded to the GPU's cursor plane only when its shape
//! changes; moving is a cheap position update done once per input batch.

/// Premultiplied ARGB8888 cursor image.
#[cfg_attr(not(feature = "zig-backend"), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub pixels: Vec<u32>,
}

/// Built-in arrow until cursor themes are loaded. `#` = outline, `.` = fill.
const ARROW: [&str; 19] = [
    "#           ",
    "##          ",
    "#.#         ",
    "#..#        ",
    "#...#       ",
    "#....#      ",
    "#.....#     ",
    "#......#    ",
    "#.......#   ",
    "#........#  ",
    "#.........# ",
    "#......#####",
    "#...#..#    ",
    "#..# #..#   ",
    "#.#  #..#   ",
    "##    #..#  ",
    "#     #..#  ",
    "       #..# ",
    "       ###  ",
];

const OUTLINE: u32 = 0xff20_1a17; // near-black, warm
const FILL: u32 = 0xffff_ffff;

impl CursorImage {
    /// The built-in arrow, scaled by an integer factor (1 = 12x19).
    pub fn arrow(scale: u32) -> Self {
        let scale = scale.max(1);
        let (bw, bh) = (ARROW[0].len() as u32, ARROW.len() as u32);
        let (width, height) = (bw * scale, bh * scale);
        let mut pixels = vec![0u32; (width * height) as usize];
        for y in 0..height {
            let row = ARROW[(y / scale) as usize].as_bytes();
            for x in 0..width {
                pixels[(y * width + x) as usize] = match row[(x / scale) as usize] {
                    b'#' => OUTLINE,
                    b'.' => FILL,
                    _ => 0, // fully transparent
                };
            }
        }
        Self { width, height, hot_x: 0, hot_y: 0, pixels }
    }

    /// Largest integer scale that still fits the GPU's cursor plane.
    pub fn arrow_for(screen_height: u32, plane: (u32, u32)) -> Self {
        let wanted = (screen_height / 1080).max(1);
        let fits = (plane.0 / ARROW[0].len() as u32).min(plane.1 / ARROW.len() as u32).max(1);
        Self::arrow(wanted.min(fits))
    }
}

/// Pointer position on one screen, kept inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    pub x: f64,
    pub y: f64,
    width: f64,
    height: f64,
}

impl Pointer {
    /// Starts in the middle of a `width` x `height` screen.
    pub fn new(width: u32, height: u32) -> Self {
        let (w, h) = (width as f64, height as f64);
        Self { x: w / 2.0, y: h / 2.0, width: w, height: h }
    }

    pub fn move_by(&mut self, dx: f64, dy: f64) {
        self.x = (self.x + dx).clamp(0.0, self.width - 1.0);
        self.y = (self.y + dy).clamp(0.0, self.height - 1.0);
    }

    /// `fx`, `fy` are 0..1 fractions of the screen.
    pub fn move_to_fraction(&mut self, fx: f64, fy: f64) {
        self.x = (fx * self.width).clamp(0.0, self.width - 1.0);
        self.y = (fy * self.height).clamp(0.0, self.height - 1.0);
    }

    /// Whole-pixel position for the cursor plane.
    pub fn pixel(&self) -> (i32, i32) {
        (self.x.floor() as i32, self.y.floor() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_is_premultiplied_and_transparent_outside() {
        let img = CursorImage::arrow(1);
        assert_eq!((img.width, img.height), (12, 19));
        assert_eq!(img.pixels.len(), 12 * 19);
        // Top-right corner is outside the arrow: fully transparent, no junk.
        assert_eq!(img.pixels[11], 0);
        // Premultiplied: any pixel with zero alpha must be all zero.
        assert!(img.pixels.iter().all(|&p| p >> 24 != 0 || p == 0));
    }

    #[test]
    fn arrow_scales_and_fits_the_plane() {
        assert_eq!(CursorImage::arrow(2).width, 24);
        let img = CursorImage::arrow_for(4320, (64, 64)); // 8K wants 4x, plane fits 3x
        assert!(img.width <= 64 && img.height <= 64);
        assert_eq!(img.height, 19 * 3);
        assert_eq!(CursorImage::arrow_for(1440, (64, 64)).width, 12);
    }

    #[test]
    fn pointer_stays_on_screen() {
        let mut p = Pointer::new(1920, 1080);
        assert_eq!(p.pixel(), (960, 540));
        p.move_by(-5000.0, -5000.0);
        assert_eq!(p.pixel(), (0, 0));
        p.move_by(99999.0, 99999.0);
        assert_eq!(p.pixel(), (1919, 1079));
        p.move_to_fraction(0.5, 0.25);
        assert_eq!(p.pixel(), (960, 270));
        p.move_to_fraction(1.0, 1.0);
        assert_eq!(p.pixel(), (1919, 1079));
    }

    #[test]
    fn sub_pixel_motion_accumulates() {
        let mut p = Pointer::new(100, 100);
        for _ in 0..4 {
            p.move_by(0.25, 0.0);
        }
        assert_eq!(p.pixel(), (51, 50));
    }
}
