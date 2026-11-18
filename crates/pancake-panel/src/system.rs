//! System status and controls for the panel: media (MPRIS via playerctl),
//! volume (wpctl), network (nmcli), Bluetooth (bluetoothctl), brightness
//! (brightnessctl) and power (loginctl).
//!
//! These are standard command-line tools found on most distros, whatever
//! the init system. Anything missing is simply hidden. Polling and actions
//! run on background threads so the bar never stutters.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub player: String,
    /// MPRIS instance (e.g. "firefox.instance_1_36"), for controlling it.
    pub instance: String,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    /// Album art on disk (downloaded and cached if it was a URL).
    pub art: Option<PathBuf>,
    /// Seconds.
    pub length: f64,
    pub position: f64,
    pub sampled: Instant,
}

impl Media {
    /// Position now (it keeps moving between samples while playing).
    pub fn position_now(&self) -> f64 {
        let p = if self.playing { self.position + self.sampled.elapsed().as_secs_f64() } else { self.position };
        if self.length > 0.0 { p.min(self.length) } else { p }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Net {
    Wifi { ssid: String, signal: u32 },
    Ethernet,
    Offline,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    /// "+5°C"
    pub temp: String,
    pub condition: String,
    pub humidity: String,
    pub location: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    pub weather: Option<Weather>,
    pub media: Option<Media>,
    /// 0..1 (can exceed 1 when boosted), muted.
    pub volume: Option<(f32, bool)>,
    pub net: Option<Net>,
    /// Wi-Fi radio on (None: no Wi-Fi control).
    pub wifi_enabled: Option<bool>,
    /// Bluetooth powered (None: no Bluetooth).
    pub bluetooth: Option<bool>,
    /// 0..1 (None: no backlight).
    pub brightness: Option<f32>,
    pub can_power: bool,
}

#[derive(Debug, Clone)]
pub enum Cmd {
    PlayPause,
    Next,
    Previous,
    SetVolume(f32),
    ToggleMute,
    ToggleWifi,
    ToggleBluetooth,
    SetBrightness(f32),
    Suspend,
    Reboot,
    PowerOff,
}

fn have(tool: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file()))
        .unwrap_or(false)
}

fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run(cmd: &str, args: &[&str]) {
    let _ = Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

pub struct System {
    pub info: Arc<Mutex<Info>>,
    wake: Arc<AtomicBool>,
    /// Weather wanted, and where (empty: by IP). Changes trigger a fetch.
    weather_want: Arc<Mutex<(bool, String)>>,
}

impl System {
    /// Start polling in the background.
    pub fn start() -> Self {
        let info = Arc::new(Mutex::new(Info::default()));
        let wake = Arc::new(AtomicBool::new(true));
        let weather_want = Arc::new(Mutex::new((false, String::new())));
        let (i, w) = (info.clone(), wake.clone());
        // Weather on its own slow thread: every 15 minutes, or when the
        // setting changes.
        let (wi, ww) = (info.clone(), weather_want.clone());
        std::thread::spawn(move || {
            let mut last: Option<((bool, String), Instant)> = None;
            loop {
                let want = ww.lock().unwrap().clone();
                let stale = last.as_ref().map_or(true, |(w, t)| *w != want || t.elapsed() > Duration::from_secs(900));
                if stale {
                    let got = if want.0 && have("curl") { fetch_weather(&want.1) } else { None };
                    // Keep showing the old reading if a refresh fails.
                    if got.is_some() || !want.0 {
                        wi.lock().unwrap().weather = got;
                    }
                    last = Some((want, Instant::now()));
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
        std::thread::spawn(move || {
            let mut art_cache: Option<(String, Option<PathBuf>)> = None;
            loop {
                poll_into(&i, &mut art_cache);
                // Sleep about a second, or less if an action asked for news.
                for _ in 0..10 {
                    if w.swap(false, Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Self { info, wake, weather_want }
    }

    /// Weather on/off and location, from the config.
    pub fn set_weather(&self, on: bool, location: &str) {
        *self.weather_want.lock().unwrap() = (on, location.to_string());
    }

    pub fn snapshot(&self) -> Info {
        self.info.lock().unwrap().clone()
    }

    /// Do something, in the background; status refreshes right after.
    pub fn act(&self, cmd: Cmd) {
        let wake = self.wake.clone();
        // Control the player we're showing, not whichever is first.
        let player = self.info.lock().unwrap().media.as_ref().map(|m| m.instance.clone());
        // Update what we show right away, so controls feel instant.
        {
            let mut i = self.info.lock().unwrap();
            match &cmd {
                Cmd::SetVolume(v) => {
                    if let Some(vol) = &mut i.volume {
                        vol.0 = *v;
                    }
                }
                Cmd::SetBrightness(b) => i.brightness = Some(*b),
                Cmd::ToggleWifi => i.wifi_enabled = i.wifi_enabled.map(|e| !e),
                Cmd::ToggleBluetooth => i.bluetooth = i.bluetooth.map(|e| !e),
                Cmd::PlayPause => {
                    if let Some(m) = &mut i.media {
                        m.position = m.position_now();
                        m.sampled = Instant::now();
                        m.playing = !m.playing;
                    }
                }
                _ => {}
            }
        }
        std::thread::spawn(move || {
            match cmd {
                Cmd::PlayPause => player_cmd(&player, "play-pause"),
                Cmd::Next => player_cmd(&player, "next"),
                Cmd::Previous => player_cmd(&player, "previous"),
                Cmd::SetVolume(v) => run("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{:.2}", v.clamp(0.0, 1.0))]),
                Cmd::ToggleMute => run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]),
                Cmd::ToggleWifi => {
                    let on = output("nmcli", &["radio", "wifi"]).is_some_and(|s| s.trim() == "enabled");
                    run("nmcli", &["radio", "wifi", if on { "off" } else { "on" }]);
                }
                Cmd::ToggleBluetooth => {
                    let on = output("bluetoothctl", &["show"]).is_some_and(|s| s.contains("Powered: yes"));
                    run("bluetoothctl", &["power", if on { "off" } else { "on" }]);
                }
                Cmd::SetBrightness(b) => run("brightnessctl", &["set", &format!("{}%", (b.clamp(0.01, 1.0) * 100.0).round())]),
                Cmd::Suspend => run("loginctl", &["suspend"]),
                Cmd::Reboot => run("loginctl", &["reboot"]),
                Cmd::PowerOff => run("loginctl", &["poweroff"]),
            }
            wake.store(true, Ordering::Relaxed);
        });
    }
}

/// Check everything, publishing each result as soon as it's in, so one
/// slow tool doesn't hold up the rest. Fast ones go first.
fn poll_into(info: &Mutex<Info>, art_cache: &mut Option<(String, Option<PathBuf>)>) {
    info.lock().unwrap().can_power = have("loginctl");
    if have("wpctl") {
        let v = poll_volume();
        info.lock().unwrap().volume = v;
    }
    if have("playerctl") {
        let mut new = poll_media(art_cache);
        let mut i = info.lock().unwrap();
        // Keep the old position sample if nothing jumped, so the progress
        // bar moves smoothly between polls.
        if let (Some(o), Some(n)) = (&i.media, &mut new) {
            if o.title == n.title && o.playing == n.playing && (o.position_now() - n.position).abs() < 2.0 {
                n.position = o.position;
                n.sampled = o.sampled;
            }
        }
        i.media = new;
    }
    if have("brightnessctl") && has_backlight() {
        let b = poll_brightness();
        info.lock().unwrap().brightness = b;
    }
    if have("bluetoothctl") {
        let b = poll_bluetooth();
        info.lock().unwrap().bluetooth = b;
    }
    if have("nmcli") {
        let w = output("nmcli", &["radio", "wifi"]).map(|s| s.trim() == "enabled");
        info.lock().unwrap().wifi_enabled = w;
        let n = poll_net();
        info.lock().unwrap().net = n;
    }
}

fn player_cmd(player: &Option<String>, what: &str) {
    match player {
        Some(p) => run("playerctl", &["-p", p, what]),
        None => run("playerctl", &[what]),
    }
}

fn poll_media(art_cache: &mut Option<(String, Option<PathBuf>)>) -> Option<Media> {
    let fmt = "{{playerName}}\t{{status}}\t{{artist}}\t{{title}}\t{{mpris:artUrl}}\t{{mpris:length}}\t{{position}}\t{{playerInstance}}";
    let out = output("playerctl", &["-a", "metadata", "--format", fmt])?;
    let mut media = pick_player(&out)?;
    let url = media.art.take().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    // Art: local files as they are; URLs downloaded once (needs curl).
    if art_cache.as_ref().map(|(u, _)| u != &url).unwrap_or(true) {
        let path = if let Some(p) = url.strip_prefix("file://") {
            Some(PathBuf::from(p))
        } else if url.starts_with("http") && have("curl") {
            download_art(&url)
        } else {
            None
        };
        *art_cache = Some((url, path));
    }
    media.art = art_cache.as_ref().and_then(|(_, p)| p.clone());
    Some(media)
}

/// Of all players (one line each): the one playing, else the first
/// paused one. So Spotify shows up even when a browser tab is open too.
pub fn pick_player(out: &str) -> Option<Media> {
    let all: Vec<Media> = out.lines().filter_map(parse_media).collect();
    let i = all.iter().position(|m| m.playing).or(if all.is_empty() { None } else { Some(0) })?;
    all.into_iter().nth(i)
}

/// One line of the playerctl format above. Art comes back as the raw URL
/// (in `art`) for the caller to resolve.
pub fn parse_media(line: &str) -> Option<Media> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() < 7 {
        return None;
    }
    let status = f[1].trim();
    if status != "Playing" && status != "Paused" {
        return None;
    }
    let us = |s: &str| s.trim().parse::<f64>().unwrap_or(0.0) / 1_000_000.0;
    Some(Media {
        player: f[0].trim().to_string(),
        instance: f.get(7).map(|i| i.trim().to_string()).filter(|i| !i.is_empty()).unwrap_or_else(|| f[0].trim().to_string()),
        playing: status == "Playing",
        artist: f[2].trim().to_string(),
        title: f[3].trim().to_string(),
        art: (!f[4].trim().is_empty()).then(|| PathBuf::from(f[4].trim())),
        length: us(f[5]),
        position: us(f[6]),
        sampled: Instant::now(),
    })
}

fn download_art(url: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?
        .join("pancake/art");
    std::fs::create_dir_all(&dir).ok()?;
    let key = url.bytes().fold(5381u64, |h, b| h.wrapping_mul(33) ^ b as u64);
    let path = dir.join(format!("{key:016x}"));
    if !path.exists() {
        let ok = Command::new("curl")
            .args(["-sfL", "--max-time", "5", "-o"])
            .arg(&path)
            .arg(url)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            let _ = std::fs::remove_file(&path);
            return None;
        }
    }
    Some(path)
}

/// From wttr.in: "+5°C|Light rain|80%|Zurich" (metric). Needs network.
fn fetch_weather(location: &str) -> Option<Weather> {
    let loc: String = location.chars().map(|c| if c == ' ' { '+' } else { c }).filter(|c| c.is_alphanumeric() || "+-,.".contains(*c)).collect();
    let url = format!("https://wttr.in/{loc}?format=%t|%C|%h|%l&m");
    parse_weather(&output("curl", &["-sf", "--max-time", "8", &url])?)
}

pub fn parse_weather(s: &str) -> Option<Weather> {
    let f: Vec<&str> = s.trim().split('|').collect();
    if f.len() < 4 || !f[0].contains('°') {
        return None;
    }
    Some(Weather {
        temp: f[0].trim().trim_start_matches('+').to_string(),
        condition: f[1].trim().to_string(),
        humidity: f[2].trim().to_string(),
        location: f[3].trim().split(',').next().unwrap_or("").to_string(),
    })
}

fn poll_volume() -> Option<(f32, bool)> {
    parse_volume(&output("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?)
}

/// "Volume: 0.45 [MUTED]"
pub fn parse_volume(s: &str) -> Option<(f32, bool)> {
    let v = s.split_whitespace().nth(1)?.parse().ok()?;
    Some((v, s.contains("MUTED")))
}

fn poll_net() -> Option<Net> {
    // "--rescan no": don't trigger a slow Wi-Fi scan just to read status.
    let wifi = output("nmcli", &["-t", "-f", "ACTIVE,SSID,SIGNAL", "dev", "wifi", "list", "--rescan", "no"]).unwrap_or_default();
    if let Some(n) = parse_active_wifi(&wifi) {
        return Some(n);
    }
    let devs = output("nmcli", &["-t", "-f", "TYPE,STATE", "dev"])?;
    if devs.lines().any(|l| l.starts_with("ethernet:connected")) {
        return Some(Net::Ethernet);
    }
    Some(Net::Offline)
}

/// From `nmcli -t -f ACTIVE,SSID,SIGNAL dev wifi`: the "yes" line.
pub fn parse_active_wifi(s: &str) -> Option<Net> {
    let line = s.lines().find(|l| l.starts_with("yes:"))?;
    let mut parts = line.splitn(2, ':').nth(1)?.rsplitn(2, ':');
    let signal = parts.next()?.parse().ok()?;
    let ssid = parts.next().unwrap_or("").replace("\\:", ":");
    Some(Net::Wifi { ssid, signal })
}

fn poll_bluetooth() -> Option<bool> {
    let out = output("bluetoothctl", &["show"])?;
    if !out.contains("Powered:") {
        return None; // no adapter
    }
    Some(out.contains("Powered: yes"))
}

fn has_backlight() -> bool {
    std::fs::read_dir("/sys/class/backlight").map(|mut d| d.next().is_some()).unwrap_or(false)
}

fn poll_brightness() -> Option<f32> {
    // "intel_backlight,backlight,500,40%,1200"
    let out = output("brightnessctl", &["-m"])?;
    let pct: f32 = out.lines().next()?.split(',').nth(3)?.trim_end_matches('%').parse().ok()?;
    Some(pct / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_line() {
        let m = parse_media("spotify\tPlaying\tDaft Punk\tOne More Time\thttps://i.scdn.co/x\t320000000\t60000000").unwrap();
        assert_eq!(m.player, "spotify");
        assert!(m.playing);
        assert_eq!(m.title, "One More Time");
        assert_eq!(m.length, 320.0);
        assert_eq!(m.position, 60.0);
        assert!(parse_media("spotify\tStopped\ta\tb\t\t0\t0").is_none());
        assert!(parse_media("short").is_none());
    }

    #[test]
    fn weather_line() {
        let w = parse_weather("+5°C|Light rain|80%|Zurich, Switzerland\n").unwrap();
        assert_eq!(w.temp, "5°C");
        assert_eq!(w.condition, "Light rain");
        assert_eq!(w.location, "Zurich");
        assert!(parse_weather("Unknown location").is_none());
    }

    #[test]
    fn playing_player_wins_over_paused_one() {
        let out = "firefox\tPaused\ta\tTab\t\t0\t0\tfirefox.instance_1\nspotify\tPlaying\tb\tSong\t\t1000000\t0\tspotify\n";
        let m = pick_player(out).unwrap();
        assert_eq!((m.player.as_str(), m.instance.as_str()), ("spotify", "spotify"));
        // Nothing playing: first paused one.
        assert_eq!(pick_player("a\tPaused\tx\tT\t\t0\t0\ta\nb\tPaused\tx\tU\t\t0\t0\tb\n").unwrap().player, "a");
    }

    #[test]
    fn volume_line() {
        assert_eq!(parse_volume("Volume: 0.45"), Some((0.45, false)));
        assert_eq!(parse_volume("Volume: 0.30 [MUTED]"), Some((0.3, true)));
        assert_eq!(parse_volume("garbage"), None);
    }

    #[test]
    fn wifi_line() {
        let s = "no:Neighbour:40\nyes:Home\\:5G:72\n";
        assert_eq!(parse_active_wifi(s), Some(Net::Wifi { ssid: "Home:5G".into(), signal: 72 }));
        assert_eq!(parse_active_wifi("no:x:1\n"), None);
    }
}

#[cfg(test)]
mod live {
    /// Manual check against the real system:
    /// cargo test -p pancake-panel live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_status() {
        let s = super::System::start();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        println!("{:#?}", s.snapshot());
    }
}
