//! System stats for the panel: CPU and memory use, battery. Read straight
//! from /proc and /sys (Linux), no daemons needed.

pub struct Stats {
    /// 0..1, since the last refresh.
    pub cpu: Option<f32>,
    /// 0..1 of RAM in use.
    pub mem: Option<f32>,
    /// 0..1 charge, and whether it's charging.
    pub battery: Option<(f32, bool)>,
    /// CPU temperature, °C.
    pub temp: Option<f32>,
    /// 0..1 of the root filesystem used.
    pub disk: Option<f32>,
    last_cpu: Option<(u64, u64)>, // (busy, total) jiffies
}

impl Stats {
    pub fn new() -> Self {
        let mut s = Self { cpu: None, mem: None, battery: None, temp: None, disk: None, last_cpu: None };
        s.refresh();
        s
    }

    pub fn refresh(&mut self) {
        if let Some((busy, total)) = read_cpu() {
            if let Some((b0, t0)) = self.last_cpu {
                let dt = total.saturating_sub(t0);
                if dt > 0 {
                    self.cpu = Some(busy.saturating_sub(b0) as f32 / dt as f32);
                }
            }
            self.last_cpu = Some((busy, total));
        }
        self.mem = read_mem();
        self.battery = read_battery();
        self.temp = pancake_fetch::temperature();
        self.disk = pancake_fetch::disk().map(|(u, t)| u as f32 / t.max(1) as f32);
    }

    /// For "did anything change" checks (rounded to whole percents).
    pub fn summary(&self) -> (Option<u32>, Option<u32>, Option<(u32, bool)>) {
        let pct = |v: f32| (v * 100.0).round() as u32;
        (self.cpu.map(pct), self.mem.map(pct), self.battery.map(|(b, c)| (pct(b), c)))
    }
}

fn read_cpu() -> Option<(u64, u64)> {
    parse_cpu(&std::fs::read_to_string("/proc/stat").ok()?)
}

/// From the first line of /proc/stat: (busy, total) time.
pub fn parse_cpu(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().next()?;
    let vals: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if vals.len() < 4 {
        return None;
    }
    let total: u64 = vals.iter().sum();
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0); // idle + iowait
    Some((total - idle, total))
}

fn read_mem() -> Option<f32> {
    parse_mem(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

pub fn parse_mem(info: &str) -> Option<f32> {
    let field = |name: &str| {
        info.lines().find(|l| l.starts_with(name)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok())
    };
    let total = field("MemTotal:")?;
    let avail = field("MemAvailable:")?;
    (total > 0.0).then(|| ((total - avail) / total) as f32)
}

fn read_battery() -> Option<(f32, bool)> {
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for e in dir.flatten() {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let cap: f32 = std::fs::read_to_string(p.join("capacity")).ok()?.trim().parse().ok()?;
        let st = std::fs::read_to_string(p.join("status")).unwrap_or_default();
        return Some((cap / 100.0, st.trim() == "Charging"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_line() {
        // user nice system idle iowait irq softirq
        assert_eq!(parse_cpu("cpu  100 0 50 800 50 0 0\ncpu0 1 2 3 4\n"), Some((150, 1000)));
        assert_eq!(parse_cpu("garbage"), None);
    }

    #[test]
    fn memory() {
        let m = parse_mem("MemTotal:       16000000 kB\nMemFree: 1 kB\nMemAvailable:    4000000 kB\n").unwrap();
        assert!((m - 0.75).abs() < 1e-6);
    }
}
