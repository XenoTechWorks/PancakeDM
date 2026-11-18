//! PancakeDM configuration: everything the user can change lives here.

use std::path::PathBuf;

/// Where the user's config lives: `$XDG_CONFIG_HOME/pancake` or `~/.config/pancake`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pancake")
}
