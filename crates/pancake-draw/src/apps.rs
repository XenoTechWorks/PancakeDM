//! Installed apps, from freedesktop `.desktop` files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct App {
    /// Desktop file id, e.g. "org.gnome.Nautilus.desktop".
    pub id: String,
    pub name: String,
    /// Command line with field codes (%f, %U, ...) removed.
    pub exec: String,
    /// Icon name (looked up in the icon theme) or absolute path.
    pub icon: Option<String>,
    /// StartupWMClass: helps match windows (by app id) to this entry.
    pub wm_class: Option<String>,
}

/// Where `applications/` folders live, most important first: the user's
/// own, then system ones, then Flatpak exports.
pub fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => dirs.push(PathBuf::from(d)),
        None => dirs.extend(home.iter().map(|h| h.join(".local/share"))),
    }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs.extend(home.iter().map(|h| h.join(".local/share/flatpak/exports/share")));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// Every app to show, sorted by name. If two folders have the same desktop
/// file id, the more important folder wins (so user overrides work).
pub fn load_all() -> Vec<App> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in data_dirs() {
        let base = dir.join("applications");
        let mut files = Vec::new();
        collect(&base, &base, &mut files, 0);
        for (id, path) in files {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(app) = std::fs::read_to_string(&path).ok().and_then(|t| parse(&id, &t)) {
                apps.push(app);
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Desktop files under `dir`; ids use '-' for subfolders (kde/foo -> kde-foo).
fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: u32) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() && depth < 3 {
            collect(base, &path, out, depth + 1);
        } else if path.extension().is_some_and(|x| x == "desktop") {
            if let Ok(rel) = path.strip_prefix(base) {
                out.push((rel.to_string_lossy().replace('/', "-"), path));
            }
        }
    }
}

/// Parse the [Desktop Entry] section. None for things that shouldn't be
/// listed: hidden, NoDisplay, not an application, or no command.
pub fn parse(id: &str, text: &str) -> Option<App> {
    let (mut in_entry, mut name, mut exec, mut kind, mut icon, mut wm_class) = (false, None, None, None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "Name" => name = Some(v.trim().to_string()),
            "Exec" => exec = Some(v.trim().to_string()),
            "Type" => kind = Some(v.trim().to_string()),
            "Icon" => icon = Some(v.trim().to_string()).filter(|i| !i.is_empty()),
            "StartupWMClass" => wm_class = Some(v.trim().to_string()),
            "NoDisplay" | "Hidden" if v.trim() == "true" => return None,
            _ => {}
        }
    }
    if kind.as_deref() != Some("Application") {
        return None;
    }
    let exec = strip_field_codes(&exec?);
    if exec.is_empty() {
        return None;
    }
    Some(App { id: id.to_string(), name: name?, exec, icon, wm_class })
}

/// The entry for a window's app id: by desktop file id, StartupWMClass,
/// or the command's program name (case-insensitive).
pub fn for_app_id<'a>(apps: &'a [App], app_id: &str) -> Option<&'a App> {
    if app_id.is_empty() {
        return None;
    }
    let id = app_id.to_lowercase();
    let program = |a: &App| {
        a.exec.split_whitespace().next().map(|p| p.rsplit('/').next().unwrap_or(p).to_lowercase()).unwrap_or_default()
    };
    apps.iter()
        .find(|a| a.id.to_lowercase() == format!("{id}.desktop"))
        .or_else(|| apps.iter().find(|a| a.wm_class.as_deref().is_some_and(|w| w.to_lowercase() == id)))
        .or_else(|| apps.iter().find(|a| program(a) == id))
}

/// Remove %f %U etc. (we launch without files); "%%" becomes "%".
pub fn strip_field_codes(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('%') => out.push('%'),
                _ => {} // drop the field code
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Indexes of apps matching `query`, best first: name starts with it, then
/// a word in the name starts with it, then it appears anywhere in the name
/// or command. Ties keep alphabetical order.
pub fn search(apps: &[App], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..apps.len()).collect();
    }
    let mut ranked: Vec<(u8, usize)> = apps
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let name = a.name.to_lowercase();
            let rank = if name.starts_with(&q) {
                0
            } else if name.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) {
                1
            } else if name.contains(&q) || a.exec.to_lowercase().contains(&q) {
                2
            } else {
                return None;
            };
            Some((rank, i))
        })
        .collect();
    ranked.sort();
    ranked.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, exec: &str) -> App {
        App { id: format!("{name}.desktop"), name: name.into(), exec: exec.into(), icon: None, wm_class: None }
    }

    #[test]
    fn matches_windows_to_entries() {
        let mut files = app("Files", "nautilus --new-window");
        files.id = "org.gnome.Nautilus.desktop".into();
        files.icon = Some("org.gnome.Nautilus".into());
        let mut vol = app("Volume Control", "pavucontrol");
        vol.wm_class = Some("org.pulseaudio.pavucontrol".into());
        let apps = vec![files, vol];
        assert_eq!(for_app_id(&apps, "org.gnome.Nautilus").unwrap().name, "Files");
        assert_eq!(for_app_id(&apps, "org.pulseaudio.pavucontrol").unwrap().name, "Volume Control");
        assert_eq!(for_app_id(&apps, "nautilus").unwrap().name, "Files");
        assert!(for_app_id(&apps, "").is_none());
    }

    #[test]
    fn parses_desktop_entries() {
        let text = "[Desktop Entry]\nType=Application\nName=Files\nName[de]=Dateien\nExec=nautilus --new-window %U\n\n[Desktop Action new]\nName=New Window\nExec=other\n";
        let a = parse("org.gnome.Nautilus.desktop", &text.replace("Type=Application\n", "Type=Application\nIcon=org.gnome.Nautilus\n")).unwrap();
        assert_eq!(a.name, "Files");
        assert_eq!(a.icon.as_deref(), Some("org.gnome.Nautilus"));
        assert_eq!(a.exec, "nautilus --new-window");
    }

    #[test]
    fn skips_hidden_and_non_apps() {
        assert!(parse("x", "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=true\n").is_none());
        assert!(parse("x", "[Desktop Entry]\nType=Link\nName=X\nURL=http://x\n").is_none());
        assert!(parse("x", "[Desktop Entry]\nType=Application\nName=X\n").is_none());
    }

    #[test]
    fn strips_field_codes() {
        assert_eq!(strip_field_codes("foo %f --bar %U"), "foo --bar");
        assert_eq!(strip_field_codes("printf 100%% %k"), "printf 100%");
    }

    #[test]
    fn search_ranks_prefix_then_word_then_anywhere() {
        // Alphabetical, as load_all returns them.
        let apps = vec![app("Contacts", "contacts"), app("Terminal", "foot"), app("Text Editor", "gedit"), app("Volume Control", "pavucontrol")];
        let names = |q: &str| search(&apps, q).into_iter().map(|i| apps[i].name.clone()).collect::<Vec<_>>();
        assert_eq!(names("te"), vec!["Terminal", "Text Editor"]); // both prefix: alphabetical
        assert_eq!(names("con"), vec!["Contacts", "Volume Control"]); // prefix, then word
        assert_eq!(names("pavu"), vec!["Volume Control"]); // command match
        assert_eq!(search(&apps, "").len(), 4);
    }
}
