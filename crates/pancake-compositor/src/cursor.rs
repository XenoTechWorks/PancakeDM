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

/// Placeholder cursor until real cursor icons exist: a solid black box.
const BOX_SIZE: u32 = 16;
const BOX_COLOR: u32 = 0xff00_0000; // opaque black (premultiplied)

impl CursorImage {
    /// The placeholder box, scaled by an integer factor (1 = 16x16).
    /// The hotspot is the top-left corner.
    pub fn placeholder(scale: u32) -> Self {
        let size = BOX_SIZE * scale.max(1);
        Self {
            width: size,
            height: size,
            hot_x: 0,
            hot_y: 0,
            pixels: vec![BOX_COLOR; (size * size) as usize],
        }
    }

    /// Placeholder sized for the screen, never bigger than the cursor plane.
    pub fn placeholder_for(screen_height: u32, plane: (u32, u32)) -> Self {
        let wanted = (screen_height / 1080).max(1);
        let fits = (plane.0.min(plane.1) / BOX_SIZE).max(1);
        Self::placeholder(wanted.min(fits))
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

    /// The screen changed size: keep the pointer on it.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width as f64;
        self.height = height as f64;
        self.move_by(0.0, 0.0);
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
    fn placeholder_is_a_solid_black_box() {
        let img = CursorImage::placeholder(1);
        assert_eq!((img.width, img.height), (16, 16));
        assert_eq!(img.pixels.len(), 16 * 16);
        assert!(img.pixels.iter().all(|&p| p == 0xff00_0000));
    }

    #[test]
    fn placeholder_scales_and_fits_the_plane() {
        assert_eq!(CursorImage::placeholder(2).width, 32);
        assert_eq!(CursorImage::placeholder_for(1440, (64, 64)).width, 16);
        assert_eq!(CursorImage::placeholder_for(2160, (64, 64)).width, 32);
        let img = CursorImage::placeholder_for(8640, (64, 64)); // wants 8x, plane fits 4x
        assert_eq!(img.width, 64);
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
