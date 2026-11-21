//! Keybinds: "super+shift+left = swap left" style combos mapped to actions.
//! Combos use physical keys (evdev codes), like most compositors, so they
//! stay put when switching keyboard layouts.

pub const SHIFT: u8 = 1;
pub const CTRL: u8 = 2;
pub const ALT: u8 = 4;
pub const SUPER: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Terminal,
    Launcher,
    Close,
    ToggleFloating,
    Maximize,
    Focus(Dir),
    Swap(Dir),
    Workspace(u32),
    MoveToWorkspace(u32),
    Spawn(String),
    Lock,
    Quit,
}

/// Built-in binds; users override them in [keybinds].
pub const DEFAULTS: &[(&str, &str)] = &[
    ("super+return", "terminal"),
    ("super+space", "launcher"),
    ("super+q", "close"),
    ("super+v", "toggle-floating"),
    ("super+f", "maximize"),
    ("super+left", "focus left"),
    ("super+right", "focus right"),
    ("super+up", "focus up"),
    ("super+down", "focus down"),
    ("super+h", "focus left"),
    ("super+l", "focus right"),
    ("super+k", "focus up"),
    ("super+j", "focus down"),
    ("super+shift+left", "swap left"),
    ("super+shift+right", "swap right"),
    ("super+shift+up", "swap up"),
    ("super+shift+down", "swap down"),
    ("super+shift+h", "swap left"),
    ("super+shift+l", "swap right"),
    ("super+shift+k", "swap up"),
    ("super+shift+j", "swap down"),
    ("super+1", "workspace 1"),
    ("super+2", "workspace 2"),
    ("super+3", "workspace 3"),
    ("super+4", "workspace 4"),
    ("super+5", "workspace 5"),
    ("super+6", "workspace 6"),
    ("super+7", "workspace 7"),
    ("super+8", "workspace 8"),
    ("super+9", "workspace 9"),
    ("super+shift+1", "move-to-workspace 1"),
    ("super+shift+2", "move-to-workspace 2"),
    ("super+shift+3", "move-to-workspace 3"),
    ("super+shift+4", "move-to-workspace 4"),
    ("super+shift+5", "move-to-workspace 5"),
    ("super+shift+6", "move-to-workspace 6"),
    ("super+shift+7", "move-to-workspace 7"),
    ("super+shift+8", "move-to-workspace 8"),
    ("super+shift+9", "move-to-workspace 9"),
    ("super+shift+e", "quit"),
    ("ctrl+alt+l", "lock"),
];

fn dir(s: &str) -> Option<Dir> {
    match s {
        "left" => Some(Dir::Left),
        "right" => Some(Dir::Right),
        "up" => Some(Dir::Up),
        "down" => Some(Dir::Down),
        _ => None,
    }
}

pub fn parse_action(s: &str) -> Option<Action> {
    let s = s.trim();
    let (word, rest) = s.split_once(char::is_whitespace).map(|(w, r)| (w, r.trim())).unwrap_or((s, ""));
    let ws = |r: &str| r.parse::<u32>().ok().filter(|n| (1..=9).contains(n));
    Some(match (word, rest) {
        ("terminal", "") => Action::Terminal,
        ("launcher", "") => Action::Launcher,
        ("close", "") => Action::Close,
        ("toggle-floating", "") => Action::ToggleFloating,
        ("maximize", "") => Action::Maximize,
        ("quit", "") => Action::Quit,
        ("lock", "") => Action::Lock,
        ("focus", d) => Action::Focus(dir(d)?),
        ("swap", d) => Action::Swap(dir(d)?),
        ("workspace", n) => Action::Workspace(ws(n)?),
        ("move-to-workspace", n) => Action::MoveToWorkspace(ws(n)?),
        ("spawn", cmd) if !cmd.is_empty() => Action::Spawn(cmd.to_string()),
        _ => return None,
    })
}

/// evdev code for a key name.
fn key_code(name: &str) -> Option<u32> {
    const LETTERS: &str = "qwertyuiop"; // 16..=25
    const LETTERS2: &str = "asdfghjkl"; // 30..=38
    const LETTERS3: &str = "zxcvbnm"; // 44..=50
    if name.len() == 1 {
        let c = name.chars().next()?;
        if let Some(i) = LETTERS.find(c) {
            return Some(16 + i as u32);
        }
        if let Some(i) = LETTERS2.find(c) {
            return Some(30 + i as u32);
        }
        if let Some(i) = LETTERS3.find(c) {
            return Some(44 + i as u32);
        }
        if let Some(d) = c.to_digit(10) {
            return Some(if d == 0 { 11 } else { 1 + d });
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return match n {
            1..=10 => Some(58 + n),
            11 => Some(87),
            12 => Some(88),
            _ => None,
        };
    }
    Some(match name {
        "return" | "enter" => 28,
        "space" => 57,
        "tab" => 15,
        "escape" | "esc" => 1,
        "backspace" => 14,
        "delete" => 111,
        "left" => 105,
        "right" => 106,
        "up" => 103,
        "down" => 108,
        "minus" => 12,
        "equal" => 13,
        "comma" => 51,
        "period" => 52,
        "slash" => 53,
        "home" => 102,
        "end" => 107,
        "pageup" => 104,
        "pagedown" => 109,
        "print" => 99,
        _ => return None,
    })
}

pub fn parse_combo(s: &str) -> Option<(u8, u32)> {
    let mut mods = 0;
    let mut key = None;
    for part in s.to_lowercase().split('+').map(str::trim) {
        match part {
            "super" | "mod" | "logo" | "win" => mods |= SUPER,
            "shift" => mods |= SHIFT,
            "ctrl" | "control" => mods |= CTRL,
            "alt" => mods |= ALT,
            k => {
                if key.is_some() {
                    return None; // two non-modifier keys
                }
                key = Some(key_code(k)?);
            }
        }
    }
    Some((mods, key?))
}

pub struct Keybinds(Vec<(u8, u32, Action)>);

impl Keybinds {
    /// Defaults plus the user's binds (theirs win; "none" removes one).
    /// Returns warnings for lines that didn't parse.
    pub fn from_config(user: &[(String, String)]) -> (Self, Vec<String>) {
        let mut binds = Vec::new();
        let mut warnings = Vec::new();
        let set = |binds: &mut Vec<(u8, u32, Action)>, combo: (u8, u32), action: Option<Action>| {
            binds.retain(|(m, k, _)| (*m, *k) != combo);
            if let Some(a) = action {
                binds.push((combo.0, combo.1, a));
            }
        };
        for (c, a) in DEFAULTS {
            set(&mut binds, parse_combo(c).expect("default combo"), Some(parse_action(a).expect("default action")));
        }
        for (c, a) in user {
            let Some(combo) = parse_combo(c) else {
                warnings.push(format!("unknown key combo: {c}"));
                continue;
            };
            if a.trim() == "none" {
                set(&mut binds, combo, None);
            } else if let Some(action) = parse_action(a) {
                set(&mut binds, combo, Some(action));
            } else {
                warnings.push(format!("unknown action for {c}: {a}"));
            }
        }
        (Self(binds), warnings)
    }

    pub fn lookup(&self, mods: u8, key: u32) -> Option<&Action> {
        self.0.iter().find(|(m, k, _)| *m == mods && *k == key).map(|(_, _, a)| a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos() {
        assert_eq!(parse_combo("Super+Return"), Some((SUPER, 28)));
        assert_eq!(parse_combo("super+shift+left"), Some((SUPER | SHIFT, 105)));
        assert_eq!(parse_combo("ctrl+alt+t"), Some((CTRL | ALT, 20)));
        assert_eq!(parse_combo("super+1"), Some((SUPER, 2)));
        assert_eq!(parse_combo("super+0"), Some((SUPER, 11)));
        assert_eq!(parse_combo("super+f12"), Some((SUPER, 88)));
        assert_eq!(parse_combo("super+a+b"), None);
        assert_eq!(parse_combo("super"), None);
        assert_eq!(parse_combo("super+nope"), None);
    }

    #[test]
    fn actions() {
        assert_eq!(parse_action("focus left"), Some(Action::Focus(Dir::Left)));
        assert_eq!(parse_action("workspace 3"), Some(Action::Workspace(3)));
        assert_eq!(parse_action("workspace 10"), None);
        assert_eq!(parse_action("spawn foot -e htop"), Some(Action::Spawn("foot -e htop".into())));
        assert_eq!(parse_action("spawn"), None);
        assert_eq!(parse_action("dance"), None);
    }

    #[test]
    fn user_binds_override_and_remove_defaults() {
        let user = vec![("super+return".to_string(), "spawn foot".to_string()), ("super+q".to_string(), "none".to_string()), ("super+zz".to_string(), "close".to_string())];
        let (k, warnings) = Keybinds::from_config(&user);
        assert_eq!(k.lookup(SUPER, 28), Some(&Action::Spawn("foot".into())));
        assert_eq!(k.lookup(SUPER, 16), None);
        assert_eq!(k.lookup(SUPER, 2), Some(&Action::Workspace(1)));
        assert_eq!(warnings.len(), 1);
    }
}
