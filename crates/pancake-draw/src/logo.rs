//! The PancakeDM logo (data/icons/pancakedm.svg), built in, any size.

use crate::images::Image;
use std::collections::HashMap;
use std::sync::Mutex;

pub const SVG: &str = include_str!("../../../data/icons/pancakedm.svg");

static CACHE: Mutex<Option<HashMap<u32, Image>>> = Mutex::new(None);

/// The logo at `size` x `size` pixels (cached).
pub fn image(size: u32) -> Option<Image> {
    let mut guard = CACHE.lock().ok()?;
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(img) = cache.get(&size) {
        return Some(img.clone());
    }
    let img = crate::images::load_svg_bytes(SVG.as_bytes(), Some((size, size)))?;
    cache.insert(size, img.clone());
    Some(img)
}

#[cfg(test)]
mod tests {
    #[test]
    fn logo_renders() {
        let img = super::image(64).expect("logo renders");
        assert_eq!((img.w, img.h), (64, 64));
        // The butter pat is near the top middle and opaque.
        assert_eq!(img.px[(13 * 64 + 32) as usize] >> 24, 255);
        // Corners are transparent.
        assert_eq!(img.px[0] >> 24, 0);
    }
}

#[cfg(test)]
mod preview {
    /// PANCAKE_LOGO_OUT=/path/logo.ppm cargo test -p pancake-draw preview -- --ignored
    #[test]
    #[ignore]
    fn write_preview() {
        let Some(out) = std::env::var_os("PANCAKE_LOGO_OUT") else { return };
        let img = super::image(256).unwrap();
        let mut data = format!("P6 {} {} 255\n", img.w, img.h).into_bytes();
        for p in &img.px {
            // Over a dark background, like the panel.
            let a = (p >> 24) as f32 / 255.0;
            let bg = [0x1f, 0x1a, 0x17];
            for (i, s) in [16, 8, 0].iter().enumerate() {
                data.push((((p >> s) & 0xff) as f32 + bg[i] as f32 * (1.0 - a)) as u8);
            }
        }
        std::fs::write(out, data).unwrap();
    }
}
