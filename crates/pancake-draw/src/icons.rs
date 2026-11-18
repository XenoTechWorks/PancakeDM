//! Finding app icons in freedesktop icon themes, with a cache.

use crate::images::{self, Image};
use std::collections::HashMap;
use std::path::PathBuf;

const SIZES: [u32; 11] = [16, 22, 24, 32, 48, 64, 96, 128, 192, 256, 512];

/// Folders that hold icon themes, most important first.
fn bases() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(h) = &home {
        out.push(h.join(".local/share/icons"));
        out.push(h.join(".icons"));
    }
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    out.extend(data.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("icons")));
    if let Some(h) = &home {
        out.push(h.join(".local/share/flatpak/exports/share/icons"));
    }
    out.push(PathBuf::from("/var/lib/flatpak/exports/share/icons"));
    out
}

/// Size folders to try for `size`: exact, then bigger (downscaling looks
/// better), then smaller; then scalable.
fn size_dirs(size: u32) -> Vec<String> {
    let mut sizes: Vec<u32> = SIZES.iter().copied().filter(|&s| s >= size).collect();
    sizes.extend(SIZES.iter().rev().copied().filter(|&s| s < size));
    let mut dirs = Vec::new();
    for s in sizes {
        dirs.push(format!("{s}x{s}/apps"));
        dirs.push(format!("apps/{s}")); // Breeze-style layout
        dirs.push(format!("{s}x{s}@2/apps"));
    }
    dirs.push("scalable/apps".into());
    dirs.push("apps/scalable".into());
    dirs
}

/// Path of the best icon file for `name` at `size` in `theme` (then the
/// usual fallbacks, then hicolor and pixmaps).
pub fn find(name: &str, size: u32, theme: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(name);
    if direct.is_absolute() {
        return direct.exists().then_some(direct);
    }
    let mut themes = vec![theme.to_string()];
    for t in ["Adwaita", "breeze", "Papirus", "hicolor"] {
        if !themes.iter().any(|x| x == t) {
            themes.push(t.into());
        }
    }
    let bases = bases();
    let dirs = size_dirs(size);
    for t in &themes {
        for base in &bases {
            let theme_dir = base.join(t);
            if !theme_dir.is_dir() {
                continue;
            }
            for d in &dirs {
                for ext in ["png", "svg"] {
                    let p = theme_dir.join(d).join(format!("{name}.{ext}"));
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
    }
    for pix in ["/usr/share/pixmaps", "/usr/local/share/pixmaps"] {
        for ext in ["png", "svg"] {
            let p = PathBuf::from(pix).join(format!("{name}.{ext}"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Loaded icons, by (name, size). Misses are remembered too.
pub struct Icons {
    pub theme: String,
    cache: HashMap<(String, u32), Option<Image>>,
}

impl Icons {
    pub fn new(theme: &str) -> Self {
        Self { theme: theme.to_string(), cache: HashMap::new() }
    }

    pub fn set_theme(&mut self, theme: &str) {
        if theme != self.theme {
            self.theme = theme.to_string();
            self.cache.clear();
        }
    }

    pub fn get(&mut self, name: &str, size: u32) -> Option<&Image> {
        let theme = self.theme.clone();
        self.cache
            .entry((name.to_string(), size))
            .or_insert_with(|| find(name, size, &theme).and_then(|p| images::load(&p, Some((size, size)))))
            .as_ref()
    }
}
