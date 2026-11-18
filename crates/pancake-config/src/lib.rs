//! PancakeDM configuration: everything the user can change lives here.
//!
//! One file, `~/.config/pancake/pancake.conf`, in a simple format:
//!
//! ```text
//! # comments start with #
//! [desktop]
//! wallpaper = pancakes.jpg
//! wallpaper-mode = fill
//! ```
//!
//! Missing keys get defaults, unknown keys are ignored, and the compositor
//! and shell programs reload it when it changes, so edits apply live.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperMode {
    Fill,
    Fit,
    Center,
    Stretch,
}

impl WallpaperMode {
    pub const ALL: [WallpaperMode; 4] = [Self::Fill, Self::Fit, Self::Center, Self::Stretch];

    pub fn name(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Fit => "fit",
            Self::Center => "center",
            Self::Stretch => "stretch",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPosition {
    Top,
    Bottom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// File name (looked up in the wallpaper folders) or a path; empty = none.
    pub wallpaper: String,
    pub wallpaper_mode: WallpaperMode,
    /// 0xRRGGBB, shown with no wallpaper or around a "fit" one.
    pub background: u32,
    pub panel_position: PanelPosition,
    pub clock_24h: bool,
    pub show_date: bool,
    /// 0xRRGGBB, for focus and selection highlights.
    pub accent: u32,
    pub icon_theme: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wallpaper: "pancakes.jpg".into(),
            wallpaper_mode: WallpaperMode::Fill,
            background: 0xf4a259,
            panel_position: PanelPosition::Top,
            clock_24h: true,
            show_date: true,
            accent: 0xf4a259,
            icon_theme: "Adwaita".into(),
        }
    }
}

/// Accent colors offered in Settings (any #rrggbb works in the file).
pub const ACCENTS: [u32; 6] = [0xf4a259, 0xbc4b51, 0x8cb369, 0x5b8e7d, 0x3d5a80, 0x8e5b7d];

/// Where the user's config lives: `$XDG_CONFIG_HOME/pancake` or `~/.config/pancake`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pancake")
}

pub fn config_path() -> PathBuf {
    config_dir().join("pancake.conf")
}

/// Folders searched for wallpapers by name, most important first.
pub fn wallpaper_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => dirs.push(PathBuf::from(d).join("pancake/wallpapers")),
        None => dirs.extend(home.iter().map(|h| h.join(".local/share/pancake/wallpapers"))),
    }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("pancake/wallpapers")));
    dirs.extend(home.iter().map(|h| h.join("Pictures/Wallpapers")));
    dirs
}

/// Every image in the wallpaper folders.
pub fn list_wallpapers() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in wallpaper_dirs() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| ["jpg", "jpeg", "png"].iter().any(|e| x.eq_ignore_ascii_case(e))))
            .collect();
        found.sort();
        for p in found {
            if !out.iter().any(|o: &PathBuf| o.file_name() == p.file_name()) {
                out.push(p);
            }
        }
    }
    out
}

fn expand_home(s: &str) -> PathBuf {
    match (s.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(h)) => PathBuf::from(h).join(rest),
        _ => PathBuf::from(s),
    }
}

impl Config {
    /// The wallpaper file to show, if it can be found.
    pub fn wallpaper_path(&self) -> Option<PathBuf> {
        if self.wallpaper.trim().is_empty() {
            return None;
        }
        let p = expand_home(self.wallpaper.trim());
        if p.is_absolute() {
            return p.exists().then_some(p);
        }
        wallpaper_dirs().into_iter().map(|d| d.join(&p)).find(|c| c.exists())
    }
}

fn parse_color(s: &str) -> Option<u32> {
    let hex = s.trim().trim_start_matches('#');
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Comments: a line starting with '#', or " # ..." after a value. A '#'
/// right before text (like a #rrggbb color) isn't a comment.
fn strip_comment(line: &str) -> &str {
    if line.trim_start().starts_with('#') {
        return "";
    }
    let b = line.as_bytes();
    for i in 1..b.len() {
        let after_ok = b.get(i + 1).map_or(true, |c| c.is_ascii_whitespace());
        if b[i] == b'#' && b[i - 1].is_ascii_whitespace() && after_ok {
            return &line[..i];
        }
    }
    line
}

/// Parse config text. Bad values keep their default (with a warning).
pub fn parse(text: &str) -> Config {
    let mut c = Config::default();
    let mut section = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            eprintln!("pancake.conf:{}: expected key = value", n + 1);
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        let ok = match (section.as_str(), k) {
            ("desktop", "wallpaper") => {
                c.wallpaper = v.to_string();
                true
            }
            ("desktop", "wallpaper-mode") => WallpaperMode::parse(v).map(|m| c.wallpaper_mode = m).is_some(),
            ("desktop", "background") => parse_color(v).map(|x| c.background = x).is_some(),
            ("panel", "position") => match v {
                "top" => Some(PanelPosition::Top),
                "bottom" => Some(PanelPosition::Bottom),
                _ => None,
            }
            .map(|p| c.panel_position = p)
            .is_some(),
            ("panel", "clock") => match v {
                "24h" => Some(true),
                "12h" => Some(false),
                _ => None,
            }
            .map(|b| c.clock_24h = b)
            .is_some(),
            ("panel", "show-date") => parse_bool(v).map(|b| c.show_date = b).is_some(),
            ("theme", "accent") => parse_color(v).map(|x| c.accent = x).is_some(),
            ("theme", "icon-theme") => {
                c.icon_theme = v.to_string();
                true
            }
            _ => true, // unknown keys are fine (newer or older versions)
        };
        if !ok {
            eprintln!("pancake.conf:{}: bad value for {k}: {v}", n + 1);
        }
    }
    c
}

/// The config as file text.
pub fn to_text(c: &Config) -> String {
    format!(
        "# PancakeDM settings. Edit freely: changes apply as soon as you save.\n\
         # Desktop Settings (pancake-settings) writes this file too.\n\
         \n\
         [desktop]\n\
         # A file in ~/.local/share/pancake/wallpapers (or a full path); empty for none.\n\
         wallpaper = {}\n\
         # fill, fit, center or stretch\n\
         wallpaper-mode = {}\n\
         background = #{:06x}\n\
         \n\
         [panel]\n\
         # top or bottom\n\
         position = {}\n\
         # 24h or 12h\n\
         clock = {}\n\
         show-date = {}\n\
         \n\
         [theme]\n\
         accent = #{:06x}\n\
         icon-theme = {}\n",
        c.wallpaper,
        c.wallpaper_mode.name(),
        c.background,
        if c.panel_position == PanelPosition::Top { "top" } else { "bottom" },
        if c.clock_24h { "24h" } else { "12h" },
        c.show_date,
        c.accent,
        c.icon_theme,
    )
}

/// Load the user's config (defaults if there's no file).
pub fn load() -> Config {
    std::fs::read_to_string(config_path()).map(|t| parse(&t)).unwrap_or_default()
}

/// Write the config atomically (temp file, then rename), so readers never
/// see half a file.
pub fn save(c: &Config) -> std::io::Result<()> {
    let path = config_path();
    std::fs::create_dir_all(config_dir())?;
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, to_text(c))?;
    std::fs::rename(tmp, path)
}

/// Notices when the config file changes (polls its modification time).
pub struct Watcher {
    path: PathBuf,
    last: Option<SystemTime>,
}

impl Watcher {
    pub fn new() -> Self {
        let path = config_path();
        let last = mtime(&path);
        Self { path, last }
    }

    /// True once per change.
    pub fn changed(&mut self) -> bool {
        let now = mtime(&self.path);
        if now != self.last {
            self.last = now;
            return true;
        }
        false
    }
}

impl Default for Watcher {
    fn default() -> Self {
        Self::new()
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_but_not_colors() {
        assert_eq!(strip_comment("# whole line"), "");
        assert_eq!(strip_comment("a = b # note"), "a = b ");
        assert_eq!(strip_comment("accent = #bc4b51"), "accent = #bc4b51");
    }

    #[test]
    fn defaults_when_empty() {
        assert_eq!(parse(""), Config::default());
    }

    #[test]
    fn parses_sections_and_values() {
        let c = parse(
            "[desktop]\nwallpaper = ~/pics/a.png  # mine\nwallpaper-mode = fit\nbackground = #102030\n\
             [panel]\nposition = bottom\nclock = 12h\nshow-date = no\n[theme]\naccent = #bc4b51\n",
        );
        assert_eq!(c.wallpaper, "~/pics/a.png");
        assert_eq!(c.wallpaper_mode, WallpaperMode::Fit);
        assert_eq!(c.background, 0x102030);
        assert_eq!(c.panel_position, PanelPosition::Bottom);
        assert!(!c.clock_24h);
        assert!(!c.show_date);
        assert_eq!(c.accent, 0xbc4b51);
    }

    #[test]
    fn bad_values_keep_defaults_and_unknown_keys_are_ignored() {
        let c = parse("[panel]\nposition = sideways\nfuture-key = 1\n[desktop]\nbackground = red\n");
        assert_eq!(c.panel_position, PanelPosition::Top);
        assert_eq!(c.background, Config::default().background);
    }

    #[test]
    fn round_trips_through_text() {
        let mut c = Config::default();
        c.wallpaper_mode = WallpaperMode::Center;
        c.panel_position = PanelPosition::Bottom;
        c.clock_24h = false;
        c.accent = 0x3d5a80;
        assert_eq!(parse(&to_text(&c)), c);
    }
}
