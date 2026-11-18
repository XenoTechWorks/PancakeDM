//! IPC between the compositor and shell programs (panel, settings, ...).

use std::path::PathBuf;

/// Socket path: `$XDG_RUNTIME_DIR/pancake.sock`.
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("pancake.sock")
}
