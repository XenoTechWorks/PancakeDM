//! System info, read straight from /proc, /sys and /etc (plus a few
//! package databases), for `pancake-fetch` and the panel's dashboard.
//! Works the same from KISS Linux to Linux Mint: no daemons, no systemd.

use std::path::Path;
use std::process::Command;

fn read(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

/// "Linux Mint 22", "Void Linux", "KISS Linux"...
pub fn os() -> Option<String> {
    let text = read("/etc/os-release").or_else(|| read("/usr/lib/os-release"))?;
    os_from_release(&text)
}

pub fn os_from_release(text: &str) -> Option<String> {
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty());
    field("PRETTY_NAME").or_else(|| field("NAME"))
}

/// Stable distro id from os-release ("arch", "ubuntu", "void"...).
pub fn os_id() -> Option<String> {
    let text = read("/etc/os-release").or_else(|| read("/usr/lib/os-release"))?;
    text.lines().find_map(|l| l.strip_prefix("ID=")).map(|v| v.trim().trim_matches('"').to_ascii_lowercase()).filter(|v| !v.is_empty())
}

/// Icon name for the distro's logo (os-release LOGO=, e.g. "void-logo").
pub fn os_logo() -> Option<String> {
    let text = read("/etc/os-release").or_else(|| read("/usr/lib/os-release"))?;
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty());
    field("LOGO").or_else(|| field("ID").map(|id| format!("distributor-logo-{id}")))
}

pub fn kernel() -> Option<String> {
    read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string())
}

pub fn hostname() -> Option<String> {
    read("/proc/sys/kernel/hostname").or_else(|| read("/etc/hostname")).map(|s| s.trim().to_string())
}

pub fn user() -> Option<String> {
    std::env::var("USER").ok().or_else(|| std::env::var("LOGNAME").ok())
}

/// Seconds since boot.
pub fn uptime_secs() -> Option<u64> {
    read("/proc/uptime")?.split_whitespace().next()?.parse::<f64>().ok().map(|s| s as u64)
}

/// "2 hours, 5 minutes"
pub fn format_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    let unit = |n: u64, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(unit(d, "day"));
    }
    if h > 0 {
        parts.push(unit(h, "hour"));
    }
    if m > 0 || parts.is_empty() {
        parts.push(unit(m, "min"));
    }
    parts.join(", ")
}

/// "Ryzen 7 5800X (16)"
pub fn cpu() -> Option<String> {
    let info = read("/proc/cpuinfo")?;
    let model = info.lines().find_map(|l| l.strip_prefix("model name").and_then(|r| r.split_once(':')).map(|(_, v)| v.trim().to_string()))?;
    let threads = info.lines().filter(|l| l.starts_with("processor")).count();
    let model = model.replace("(R)", "").replace("(TM)", "").replace(" CPU", "");
    let model = model.split(" @").next().unwrap_or(&model).split_whitespace().collect::<Vec<_>>().join(" ");
    Some(if threads > 0 { format!("{model} ({threads})") } else { model })
}

/// Graphics card name, via lspci if it's installed.
pub fn gpu() -> Option<String> {
    let out = Command::new("lspci").arg("-mm").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("VGA") || l.contains("3D controller") || l.contains("Display controller"))?;
    // -mm: slot "class" "vendor" "device" ...
    let q: Vec<&str> = line.split('"').collect();
    let vendor = q.get(3)?.replace("Advanced Micro Devices, Inc. [AMD/ATI]", "AMD").replace(" Corporation", "");
    let device = q.get(5)?.to_string();
    let device = device.rsplit_once('[').map(|(_, n)| n.trim_end_matches(']').to_string()).unwrap_or(device);
    Some(format!("{vendor} {device}"))
}

/// (used, total) bytes of RAM.
pub fn memory() -> Option<(u64, u64)> {
    let info = read("/proc/meminfo")?;
    let kb = |k: &str| info.lines().find(|l| l.starts_with(k))?.split_whitespace().nth(1)?.parse::<u64>().ok();
    let total = kb("MemTotal:")?;
    let avail = kb("MemAvailable:")?;
    Some(((total - avail) * 1024, total * 1024))
}

/// (used, total) bytes on the root filesystem, via df.
pub fn disk() -> Option<(u64, u64)> {
    let out = Command::new("df").args(["-Pk", "/"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let f: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    let total: u64 = f.get(1)?.parse().ok()?;
    let used: u64 = f.get(2)?.parse().ok()?;
    Some((used * 1024, total * 1024))
}

/// CPU temperature in °C, from the kernel's thermal zones or hwmon.
pub fn temperature() -> Option<f32> {
    let mut best: Option<f32> = None;
    for dir in ["/sys/class/hwmon", "/sys/class/thermal"] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = std::fs::read_to_string(p.join("name")).or_else(|_| std::fs::read_to_string(p.join("type"))).unwrap_or_default();
            let cpuish = ["k10temp", "coretemp", "zenpower", "cpu", "x86_pkg_temp", "acpitz"].iter().any(|n| name.trim().contains(n));
            if !cpuish {
                continue;
            }
            for f in ["temp1_input", "temp"] {
                if let Some(v) = std::fs::read_to_string(p.join(f)).ok().and_then(|s| s.trim().parse::<f32>().ok()) {
                    let c = v / 1000.0;
                    if (1.0..150.0).contains(&c) {
                        best = Some(best.map_or(c, |b: f32| b.max(c)));
                    }
                }
            }
        }
    }
    best
}

fn count_dirs(p: &str) -> Option<usize> {
    Some(std::fs::read_dir(p).ok()?.flatten().filter(|e| e.path().is_dir()).count())
}

fn count_lines_starting(p: &str, prefix: &str) -> Option<usize> {
    Some(read(p)?.lines().filter(|l| l.starts_with(prefix)).count())
}

/// Installed packages per package manager, e.g. [("xbps", 1203), ("flatpak", 14)].
pub fn packages() -> Vec<(&'static str, usize)> {
    let mut out = Vec::new();
    let mut add = |name, n: Option<usize>| {
        if let Some(n) = n.filter(|&n| n > 0) {
            out.push((name, n));
        }
    };
    add("kiss", count_dirs("/var/db/kiss/installed"));
    add("pacman", count_dirs("/var/lib/pacman/local").map(|n| n.saturating_sub(0)));
    add("dpkg", count_lines_starting("/var/lib/dpkg/status", "Status: install ok installed"));
    add("apk", count_lines_starting("/lib/apk/db/installed", "P:"));
    // xbps: one <key>pkgver</key> per package in its database.
    if Path::new("/var/db/xbps").is_dir() {
        let n = std::fs::read_dir("/var/db/xbps")
            .ok()
            .and_then(|d| d.flatten().find(|e| e.file_name().to_string_lossy().starts_with("pkgdb-")).and_then(|e| std::fs::read_to_string(e.path()).ok()).map(|t| t.matches("<key>pkgver</key>").count()));
        add("xbps", n);
    }
    add("rpm", Command::new("rpm").arg("-qa").output().ok().map(|o| o.stdout.split(|&b| b == b'\n').filter(|l| !l.is_empty()).count()));
    let flatpaks = ["/var/lib/flatpak/app"].iter().filter_map(|p| count_dirs(p)).sum::<usize>() + std::env::var_os("HOME").and_then(|h| count_dirs(&format!("{}/.local/share/flatpak/app", h.to_string_lossy()))).unwrap_or(0);
    add("flatpak", Some(flatpaks));
    out
}

pub fn shell() -> Option<String> {
    std::env::var("SHELL").ok().map(|s| s.rsplit('/').next().unwrap_or(&s).to_string())
}

/// Human sizes: "7.4 GiB".
pub fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// Everything, as (label, value) lines in display order.
pub fn gather() -> Vec<(&'static str, String)> {
    let mut v: Vec<(&str, String)> = Vec::new();
    if let Some(o) = os() {
        v.push(("OS", o));
    }
    if let Some(k) = kernel() {
        v.push(("Kernel", k));
    }
    if let Some(u) = uptime_secs() {
        v.push(("Uptime", format_uptime(u)));
    }
    let pk = packages();
    if !pk.is_empty() {
        v.push(("Packages", pk.iter().map(|(m, n)| format!("{n} ({m})")).collect::<Vec<_>>().join(", ")));
    }
    if let Some(s) = shell() {
        v.push(("Shell", s));
    }
    v.push(("Desktop", "PancakeDM".into()));
    if let Some(c) = cpu() {
        v.push(("CPU", c));
    }
    if let Some(g) = gpu() {
        v.push(("GPU", g));
    }
    if let Some((u, t)) = memory() {
        v.push(("Memory", format!("{} / {}", gib(u), gib(t))));
    }
    if let Some((u, t)) = disk() {
        v.push(("Disk (/)", format!("{} / {}", gib(u), gib(t))));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release() {
        assert_eq!(os_from_release("NAME=\"Void\"\nPRETTY_NAME=\"Void Linux\"\n").as_deref(), Some("Void Linux"));
        assert_eq!(os_from_release("NAME=KISS\n").as_deref(), Some("KISS"));
        assert_eq!(os_from_release(""), None);
    }

    #[test]
    fn uptime_words() {
        assert_eq!(format_uptime(59), "0 mins");
        assert_eq!(format_uptime(3660), "1 hour, 1 min");
        assert_eq!(format_uptime(2 * 86400 + 5 * 60), "2 days, 5 mins");
    }
}

#[cfg(test)]
mod live {
    /// cargo test -p pancake-fetch live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_temperature() {
        println!("temperature: {:?}", super::temperature());
    }
}
