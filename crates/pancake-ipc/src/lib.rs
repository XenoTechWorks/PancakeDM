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
