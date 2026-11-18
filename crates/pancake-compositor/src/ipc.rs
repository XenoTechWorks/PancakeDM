//! The compositor's end of the control socket (see the pancake-ipc crate).
//!
//! Commands: `screenshot PATH` (PNG of screen 0's next frame),
//! `windows` (titles, one per `|`), `quit`.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

pub enum Command {
    Screenshot(PathBuf),
    Windows,
    Quit,
}

pub struct Ipc {
    listener: UnixListener,
    path: PathBuf,
}

impl Ipc {
    pub fn bind(wayland_display: &str) -> Result<Self, String> {
        let path = pancake_ipc::socket_path(wayland_display);
        // Our Wayland socket name is locked to us, so a leftover file with
        // the same name is stale.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self { listener, path })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn fd(&self) -> i32 {
        self.listener.as_raw_fd()
    }

    /// Accept pending connections and read their commands. Bad commands get
    /// an error reply right away; good ones come back with their stream so
    /// the caller can reply (now, or after a frame for screenshots).
    pub fn accept(&self) -> Vec<(Command, UnixStream)> {
        let mut out = Vec::new();
        while let Ok((stream, _)) = self.listener.accept() {
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let mut words = line.split_whitespace();
            let cmd = match (words.next(), words.next()) {
                (Some("screenshot"), Some(p)) => Command::Screenshot(PathBuf::from(p)),
                (Some("windows"), None) => Command::Windows,
                (Some("quit"), None) => Command::Quit,
                _ => {
                    reply(stream, &format!("error unknown command: {}", line.trim()));
                    continue;
                }
            };
            out.push((cmd, stream));
        }
        out
    }
}

impl Drop for Ipc {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn reply(mut stream: UnixStream, msg: &str) {
    let _ = writeln!(stream, "{msg}");
}
