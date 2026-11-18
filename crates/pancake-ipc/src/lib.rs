//! IPC between the compositor and shell programs (panel, settings, ...).
//!
//! Each running compositor listens on `$XDG_RUNTIME_DIR/pancake-<wayland
//! socket name>.sock`. The protocol is one line per connection: send a
//! command, read back one reply. Replies start with `ok` or `error`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// The control socket for the compositor serving `wayland_display`.
pub fn socket_path(wayland_display: &str) -> PathBuf {
    runtime_dir().join(format!("pancake-{wayland_display}.sock"))
}

/// Find the compositor to talk to: `$PANCAKE_SOCKET`, or the one serving
/// `$WAYLAND_DISPLAY` (true inside a PancakeDM session), or the only one
/// running.
pub fn find_socket() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("PANCAKE_SOCKET") {
        return Ok(PathBuf::from(p));
    }
    if let Some(name) = std::env::var_os("WAYLAND_DISPLAY") {
        let p = socket_path(&name.to_string_lossy());
        if p.exists() {
            return Ok(p);
        }
    }
    let found: Vec<PathBuf> = std::fs::read_dir(runtime_dir())
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            n.starts_with("pancake-") && n.ends_with(".sock")
        })
        .collect();
    match found.len() {
        0 => Err("no running PancakeDM found".into()),
        1 => Ok(found.into_iter().next().unwrap()),
        _ => Err("several PancakeDMs are running; set PANCAKE_SOCKET".into()),
    }
}

/// Send one command and return the reply line.
pub fn send(command: &str) -> Result<String, String> {
    let path = find_socket()?;
    let mut s = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    writeln!(s, "{command}").map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).map_err(|e| e.to_string())?;
    Ok(reply.trim_end().to_string())
}

/// An open window, as listed by the `windows` command.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub focused: bool,
    /// The app's id (xdg_toplevel.set_app_id), for finding its icon.
    pub app_id: String,
}

// Records are separated by ASCII RS, fields by US, so titles can contain
// anything printable.
const RS: char = '\u{1e}';
const US: char = '\u{1f}';

/// Encode a window list for the `windows` reply (after "ok ").
pub fn encode_windows(windows: &[WindowInfo]) -> String {
    windows
        .iter()
        .map(|w| {
            let clean = |s: &str| s.chars().filter(|c| *c != RS && *c != US && *c != '\n').collect::<String>();
            format!("{}{US}{}{US}{}{US}{}", w.id, w.focused as u8, clean(&w.app_id), clean(&w.title))
        })
        .collect::<Vec<_>>()
        .join(&RS.to_string())
}

pub fn decode_windows(reply: &str) -> Vec<WindowInfo> {
    let body = reply.strip_prefix("ok").unwrap_or(reply).trim_start_matches(' ');
    body.split(RS)
        .filter_map(|rec| {
            let mut f = rec.splitn(4, US);
            let id = f.next()?.parse().ok()?;
            let focused = f.next()? == "1";
            let app_id = f.next().unwrap_or("").to_string();
            Some(WindowInfo { id, focused, app_id, title: f.next().unwrap_or("").to_string() })
        })
        .collect()
}

/// Ask the compositor for its open windows.
pub fn windows() -> Result<Vec<WindowInfo>, String> {
    let reply = send("windows")?;
    if !reply.starts_with("ok") {
        return Err(reply);
    }
    Ok(decode_windows(&reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_list_round_trips() {
        let list = vec![
            WindowInfo { id: 3, title: "Files | Home".into(), focused: false, app_id: "org.gnome.Nautilus".into() },
            WindowInfo { id: 7, title: "".into(), focused: true, app_id: "".into() },
        ];
        assert_eq!(decode_windows(&format!("ok {}", encode_windows(&list))), list);
        assert_eq!(decode_windows("ok "), vec![]);
    }
}
