//! pancake-panel: the PancakeDM top bar.
//!
//! A floating, rounded bar (wlr-layer-shell) made of "widget islands":
//! the Pancake button, a workspace switcher with each workspace's app
//! icons, the focused window, now-playing media, the clock, system stats,
//! status icons (network, Bluetooth, volume) and the control center button.
//! Clicking media or the status icons opens a card (media controls, or the
//! control center with toggles, sliders and power buttons) that fades and
//! slides in. Drawn by hand, no toolkit.

mod stats;
mod system;

use chrono::Local;
use memmap2::MmapMut;
use pancake_config::{Config, PanelPosition};
use pancake_draw::color::*;
use pancake_draw::icons::Icons;
use pancake_draw::images::{self, Image};
use pancake_draw::{Canvas, Fonts};
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use system::{Cmd, Info, Net, System};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

const BAR_HEIGHT: u32 = 44;
const MARGIN: i32 = 8;
const RADIUS: f32 = 16.0;
const ISLAND_H: f32 = 32.0;
const ISLAND_R: f32 = 12.0;
const ISLAND: u32 = 0x14ffffff;
const GAP: f32 = 6.0;
const CARD_BG: u32 = 0xf51f1a17;
const OPEN_MS: f32 = 190.0;
const CLOSE_MS: f32 = 140.0;

type Rect = (f32, f32, f32, f32);

/// Which of our surfaces something is about.
const BAR: usize = 0;
const POPUP: usize = 1;

struct App {
    configured: [bool; 2],
    size: [(u32, u32); 2],
    running: bool,
    surface_ids: [Option<ObjectId>; 2],
    pointer: Option<(f64, f64)>,
    pointer_on: usize,
    clicked: Option<(usize, f64, f64)>,
    button_down: bool,
    buffer_free: [[bool; 2]; 2],
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Launcher,
    Workspace(u32),
    ControlCenter,
    MediaCard,
    PlayPause,
    Dashboard,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum PopupKind {
    Control,
    Media,
    Dashboard,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum PAction {
    /// Clicked outside the card: close it.
    Close,
    /// Clicked the card's background: nothing.
    Noop,
    Wifi,
    Bluetooth,
    Volume,
    Mute,
    Brightness,
    Settings,
    LogOut,
    Suspend,
    Reboot,
    PowerOff,
    PlayPause,
    Next,
    Previous,
}

/// A dropdown card on its own full-screen overlay surface (clicks outside
/// the card close it).
struct Popup {
    kind: PopupKind,
    surface: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    bufs: Vec<(wl_buffer::WlBuffer, MmapMut, (u32, u32))>,
    opened: Instant,
    closing: Option<Instant>,
    hits: Vec<(Rect, PAction)>,
    /// Power buttons need a second click.
    confirm: Option<PAction>,
    /// The slider being dragged, with its track rect.
    drag: Option<(PAction, Rect)>,
}

impl Popup {
    /// 0..1: how far open (eased), counting the closing fade.
    fn openness(&self) -> f32 {
        let ease = |t: f32| 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
        match self.closing {
            Some(c) => 1.0 - ease(c.elapsed().as_secs_f32() * 1000.0 / CLOSE_MS),
            None => ease(self.opened.elapsed().as_secs_f32() * 1000.0 / OPEN_MS),
        }
    }

    fn animating(&self) -> bool {
        match self.closing {
            Some(c) => c.elapsed().as_secs_f32() * 1000.0 < CLOSE_MS,
            None => self.opened.elapsed().as_secs_f32() * 1000.0 < OPEN_MS,
        }
    }

    fn finished_closing(&self) -> bool {
        self.closing.is_some() && !self.animating()
    }
}

/// The bar ducks out of the way while the display changes (scale, size):
/// it slides away, waits until changes have stopped for a moment, then
/// slides back in at the new size. Hides the reshuffle and any flicker.
#[derive(Default)]
struct Reveal {
    last_change: Option<Instant>,
    hide_started: Option<Instant>,
    show_started: Option<Instant>,
}

const HIDE_MS: f32 = 200.0;
const SHOW_MS: f32 = 260.0;
const SETTLE: Duration = Duration::from_millis(1500);

/// Close a card right away (no fade): its size and scale are stale.
fn drop_popup(popup: &mut Option<Popup>, app: &mut App) {
    if let Some(p) = popup.take() {
        p.layer.destroy();
        p.surface.destroy();
        app.surface_ids[POPUP] = None;
    }
}

impl Reveal {
    fn changed(&mut self) {
        let now = Instant::now();
        self.last_change = Some(now);
        if self.hide_started.is_none() || self.show_started.is_some() {
            self.hide_started = Some(now);
            self.show_started = None;
        }
    }

    /// Start coming back once things have been quiet long enough.
    fn tick(&mut self) {
        if let (Some(_), None, Some(c)) = (self.hide_started, self.show_started, self.last_change) {
            if c.elapsed() >= SETTLE {
                self.show_started = Some(Instant::now());
            }
        }
        if self.show_started.is_some_and(|t| t.elapsed().as_secs_f32() * 1000.0 >= SHOW_MS) {
            *self = Self::default();
        }
    }

    /// 1 = fully shown, 0 = hidden.
    fn visibility(&self) -> f32 {
        let ease = |t: f32| 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
        match (self.hide_started, self.show_started) {
            (_, Some(s)) => ease(s.elapsed().as_secs_f32() * 1000.0 / SHOW_MS),
            (Some(h), None) => 1.0 - ease(h.elapsed().as_secs_f32() * 1000.0 / HIDE_MS),
            _ => 1.0,
        }
    }

    fn animating(&self) -> bool {
        self.hide_started.is_some()
    }
}

fn place(layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, pos: PanelPosition) {
    use zwlr_layer_surface_v1::Anchor;
    match pos {
        PanelPosition::Top => {
            layer.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
            layer.set_margin(MARGIN, MARGIN, 0, MARGIN);
        }
        PanelPosition::Bottom => {
            layer.set_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right);
            layer.set_margin(0, MARGIN, MARGIN, MARGIN);
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("pancake-panel: {e}");
        std::process::exit(1);
    }
}

fn make_buffers(shm: &wl_shm::WlShm, qh: &QueueHandle<App>, w: u32, h: u32, owner: usize) -> Result<Vec<(wl_buffer::WlBuffer, MmapMut, (u32, u32))>, String> {
    (0..2)
        .map(|i| {
            let size = (w * h * 4) as u64;
            let (file, mem) = pancake_draw::shm_file(size, &format!("panel-{owner}-{i}"))?;
            let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
            let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888, qh, (owner, i));
            pool.destroy();
            Ok((buffer, mem, (w, h)))
        })
        .collect()
}

fn run() -> Result<(), String> {
    let mut fonts = Fonts::load()?;
    let conn = Connection::connect_to_env().map_err(|e| format!("can't connect to Wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).map_err(|e| format!("wl_compositor: {e}"))?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("wl_shm: {e}"))?;
    let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 =
        globals.bind(&qh, 1..=4, ()).map_err(|_| "the compositor has no layer shell (is this PancakeDM?)".to_string())?;
    let _seat: Option<wl_seat::WlSeat> = globals.bind(&qh, 1..=5, ()).ok();

    let surface = compositor.create_surface(&qh, ());
    let layer = layer_shell.get_layer_surface(&surface, None, zwlr_layer_shell_v1::Layer::Top, "panel".into(), &qh, BAR);
    let mut cfg = pancake_config::load();
    let mut accent = pancake_draw::accent::effective(&cfg);
    let mut scale = cfg.scale;
    let mut watch = pancake_config::Watcher::new();
    surface.set_buffer_scale(scale as i32);
    place(&layer, cfg.panel_position);
    layer.set_size(0, BAR_HEIGHT);
    layer.set_exclusive_zone(BAR_HEIGHT as i32);
    surface.commit();

    let apps = pancake_draw::apps::load_all();
    let mut icons = Icons::new(&cfg.icon_theme);
    let mut art = ArtCache::default();
    let mut settings: Option<std::process::Child> = None;
    let mut sys = stats::Stats::new();
    let system = System::start();
    system.set_weather(cfg.weather, &cfg.weather_location);
    // Fetch-style info for the dashboard (uptime refreshes as we draw).
    let fetch: Vec<(&'static str, String)> = pancake_fetch::gather();

    let mut app = App {
        configured: [false; 2],
        size: [(0, BAR_HEIGHT), (0, 0)],
        running: true,
        surface_ids: [Some(surface.id()), None],
        pointer: None,
        pointer_on: BAR,
        clicked: None,
        button_down: false,
        buffer_free: [[true; 2]; 2],
    };
    let mut bufs: Vec<(wl_buffer::WlBuffer, MmapMut, (u32, u32))> = Vec::new();
    let mut popup: Option<Popup> = None;
    let mut reveal = Reveal::default();
    let mut last_width = 0u32;
    let mut status = pancake_ipc::Status::default();
    let mut hits: Vec<(Rect, Target)> = Vec::new();
    let mut last_poll = Instant::now() - Duration::from_secs(10);
    let mut last_stats = Instant::now() - Duration::from_secs(10);
    let mut last_drawn = String::new();
    let fd = conn.backend().poll_fd().as_raw_fd();
    // For testing and screenshots: open a card at startup
    // (PANCAKE_PANEL_OPEN=control or media).
    let mut open_at_start = std::env::var("PANCAKE_PANEL_OPEN").ok();

    while app.running {
        let info = system.snapshot();
        if let Some(which) = open_at_start.as_deref() {
            let want = match which {
                "media" => Target::MediaCard,
                "dashboard" => Target::Dashboard,
                _ => Target::ControlCenter,
            };
            if let Some((r, _)) = hits.iter().find(|(_, t)| *t == want) {
                app.clicked = Some((BAR, (r.0 + r.2 / 2.0) as f64, (r.1 + r.3 / 2.0) as f64));
                open_at_start = None;
            }
        }

        // Clicks.
        if let Some((on, x, y)) = app.clicked.take() {
            let (x, y) = (x as f32, y as f32);
            if on == BAR {
                if let Some(&(_, t)) = hits.iter().find(|(r, _)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3) {
                    let toggle = |popup: &mut Option<Popup>, kind: PopupKind| {
                        let same = popup.as_ref().is_some_and(|p| p.kind == kind && p.closing.is_none());
                        if let Some(p) = popup.as_mut() {
                            p.closing.get_or_insert_with(Instant::now);
                        }
                        (!same).then_some(kind)
                    };
                    let open = match t {
                        Target::Launcher => {
                            drop(pancake_ipc::send("launcher"));
                            None
                        }
                        Target::Workspace(n) => {
                            drop(pancake_ipc::send(&format!("action workspace {n}")));
                            None
                        }
                        Target::ControlCenter => toggle(&mut popup, PopupKind::Control),
                        Target::MediaCard => toggle(&mut popup, PopupKind::Media),
                        Target::Dashboard => toggle(&mut popup, PopupKind::Dashboard),
                        Target::PlayPause => {
                            system.act(Cmd::PlayPause);
                            None
                        }
                    };
                    if let Some(kind) = open {
                        // Replace any closing card right away with the new one.
                        if let Some(old) = popup.take() {
                            old.layer.destroy();
                            old.surface.destroy();
                        }
                        let s = compositor.create_surface(&qh, ());
                        let l = layer_shell.get_layer_surface(&s, None, zwlr_layer_shell_v1::Layer::Top, "panel-card".into(), &qh, POPUP);
                        use zwlr_layer_surface_v1::Anchor;
                        l.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
                        l.set_exclusive_zone(-1);
                        s.set_buffer_scale(scale as i32);
                        s.commit();
                        app.surface_ids[POPUP] = Some(s.id());
                        app.configured[POPUP] = false;
                        app.buffer_free[POPUP] = [true; 2];
                        popup = Some(Popup { kind, surface: s, layer: l, bufs: Vec::new(), opened: Instant::now(), closing: None, hits: Vec::new(), confirm: None, drag: None });
                    }
                    last_poll = Instant::now() - Duration::from_secs(10);
                }
            } else if let Some(p) = popup.as_mut() {
                let hit = p.hits.iter().find(|(r, _)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3).copied();
                match hit {
                    None => {} // inside the card but not on a control
                    Some((_, PAction::Close)) => {
                        p.closing.get_or_insert_with(Instant::now);
                    }
                    Some((r, a)) => popup_action(p, a, r, x, &system, &mut settings),
                }
            }
        }
        // Dragging a slider.
        if let Some(p) = popup.as_mut() {
            if !app.button_down {
                p.drag = None;
            } else if let (Some((a, r)), Some((px, _)), POPUP) = (p.drag, app.pointer, app.pointer_on) {
                let v = ((px as f32 - r.0) / r.2).clamp(0.0, 1.0);
                match a {
                    PAction::Volume => system.act(Cmd::SetVolume(v)),
                    PAction::Brightness => system.act(Cmd::SetBrightness(v)),
                    _ => {}
                }
            }
        }

        if last_poll.elapsed() >= Duration::from_millis(300) {
            status = pancake_ipc::status().unwrap_or_default();
            last_poll = Instant::now();
            if watch.changed() {
                let new = pancake_config::load();
                if new.panel_position != cfg.panel_position {
                    place(&layer, new.panel_position);
                    surface.commit();
                }
                icons.set_theme(&new.icon_theme);
                accent = pancake_draw::accent::effective(&new);
                system.set_weather(new.weather, &new.weather_location);
                if new.scale != scale {
                    reveal.changed();
                    drop_popup(&mut popup, &mut app);
                    scale = new.scale;
                    surface.set_buffer_scale(scale as i32);
                    for b in bufs.drain(..) {
                        b.0.destroy();
                    }
                }
                cfg = new;
                last_drawn.clear();
            }
        }
        if last_stats.elapsed() >= Duration::from_secs(2) {
            sys.refresh();
            last_stats = Instant::now();
        }

        // The display changed size: duck out until it settles.
        if app.configured[BAR] && app.size[BAR].0 != last_width {
            if last_width != 0 {
                reveal.changed();
                drop_popup(&mut popup, &mut app);
            }
            last_width = app.size[BAR].0;
        }
        reveal.tick();
        let vis = reveal.visibility();

        // Bar: redraw only when something visible changed.
        let now = Local::now();
        let media_key = info.media.as_ref().map(|m| (m.title.clone(), m.artist.clone(), m.playing, m.art.clone()));
        let key = format!(
            "{:?}{:?}{}{:?}{:?}{}{:?}{:?}{:?}{:?}{:?}{:?}",
            app.size[BAR],
            status,
            now.format("%H:%M"),
            app.pointer.filter(|_| app.pointer_on == BAR).map(|(x, y)| (x as i32 / 4, y as i32 / 4)),
            cfg,
            accent,
            sys.summary(),
            media_key,
            info.volume,
            info.net,
            info.bluetooth,
            popup.as_ref().map(|p| (p.kind, p.closing.is_some())),
        ) + &format!("{}", (vis * 60.0) as i32);
        if app.configured[BAR] && key != last_drawn {
            let (w, h) = app.size[BAR];
            let (pw, ph) = (w * scale, h * scale);
            if bufs.first().map_or(true, |b| b.2 != (pw, ph)) {
                for b in bufs.drain(..) {
                    b.0.destroy();
                }
                bufs = make_buffers(&shm, &qh, pw, ph, BAR)?;
                app.buffer_free[BAR] = [true; 2];
            }
            if let Some(i) = (0..2).find(|&i| app.buffer_free[BAR][i]) {
                let px = pancake_draw::pixels(&mut bufs[i].1);
                let mut canvas = Canvas::new(px, w as i32, h as i32, scale as f32);
                let open = popup.as_ref().filter(|p| p.closing.is_none()).map(|p| p.kind);
                let mut ui = Ui { c: &mut canvas, fonts: &mut fonts, icons: &mut icons, art: &mut art, apps: &apps, cfg: &cfg, accent, pointer: app.pointer.filter(|_| app.pointer_on == BAR) };
                hits = ui.draw_bar(&status, &sys, &info, open);
                if vis < 1.0 {
                    // Slide toward the screen edge and fade; no clicks meanwhile.
                    hits.clear();
                    let px = pancake_draw::pixels(&mut bufs[i].1);
                    let (pw, ph) = (pw as usize, ph as usize);
                    let dy = (((1.0 - vis) * (h as f32 + MARGIN as f32)) * scale as f32) as usize;
                    let top = cfg.panel_position == PanelPosition::Top;
                    for y in 0..ph {
                        let row = if top { y } else { ph - 1 - y };
                        let src = if top { y + dy } else { (ph - 1 - y).wrapping_sub(dy) };
                        if src < ph {
                            px.copy_within(src * pw..src * pw + pw, row * pw);
                        } else {
                            px[row * pw..row * pw + pw].fill(0);
                        }
                    }
                    Canvas::new(px, w as i32, h as i32, scale as f32).fade_all(vis);
                }
                surface.attach(Some(&bufs[i].0), 0, 0);
                surface.damage_buffer(0, 0, pw as i32, ph as i32);
                surface.commit();
                app.buffer_free[BAR][i] = false;
                last_drawn = key;
            }
        }

        // The card: animated, so it redraws every frame while moving (and
        // a few times a second for the media progress bar).
        if popup.as_ref().is_some_and(|p| p.finished_closing()) {
            let p = popup.take().unwrap();
            p.layer.destroy();
            p.surface.destroy();
            app.surface_ids[POPUP] = None;
            last_drawn.clear();
        }
        if let (Some(p), true) = (popup.as_mut(), app.configured[POPUP]) {
            let (w, h) = app.size[POPUP];
            let (pw, ph) = (w * scale, h * scale);
            if p.bufs.first().map_or(true, |b| b.2 != (pw, ph)) {
                for b in p.bufs.drain(..) {
                    b.0.destroy();
                }
                p.bufs = make_buffers(&shm, &qh, pw, ph, POPUP)?;
                app.buffer_free[POPUP] = [true; 2];
            }
            if let Some(i) = (0..2).find(|&i| app.buffer_free[POPUP][i]) {
                let (kind, open, confirm) = (p.kind, p.openness(), p.confirm);
                let px = pancake_draw::pixels(&mut p.bufs[i].1);
                let mut canvas = Canvas::new(px, w as i32, h as i32, scale as f32);
                let mut ui = Ui { c: &mut canvas, fonts: &mut fonts, icons: &mut icons, art: &mut art, apps: &apps, cfg: &cfg, accent, pointer: app.pointer.filter(|_| app.pointer_on == POPUP) };
                let (hits, track) = ui.draw_card(kind, open, confirm, &info, &sys, &fetch);
                p.hits = hits;
                if let (Some(t), Some((a, _))) = (track, p.drag) {
                    p.drag = Some((a, t.iter().find(|(k, _)| *k == a).map(|(_, r)| *r).unwrap_or_default()));
                }
                p.surface.attach(Some(&p.bufs[i].0), 0, 0);
                p.surface.damage_buffer(0, 0, pw as i32, ph as i32);
                p.surface.commit();
                app.buffer_free[POPUP][i] = false;
            }
        }

        conn.flush().map_err(|e| e.to_string())?;
        let fast = popup.as_ref().is_some_and(|p| p.animating() || p.drag.is_some()) || reveal.animating();
        let timeout = if fast { 16 } else if popup.is_some() { 120 } else { 250 };
        if let Some(guard) = queue.prepare_read() {
            if pancake_draw::wait_readable(fd, timeout) {
                // "Would block" just means nothing arrived after all; only
                // real errors end the program.
                if let Err(e) = guard.read() {
                    let would_block = matches!(&e, wayland_client::backend::WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock);
                    if !would_block {
                        return Err(e.to_string());
                    }
                }
            }
        }
        queue.dispatch_pending(&mut app).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Something in a card was clicked.
fn popup_action(p: &mut Popup, a: PAction, r: Rect, x: f32, system: &System, settings: &mut Option<std::process::Child>) {
    let power = matches!(a, PAction::LogOut | PAction::Suspend | PAction::Reboot | PAction::PowerOff);
    if power && p.confirm != Some(a) {
        p.confirm = Some(a); // first click: ask again
        return;
    }
    p.confirm = None;
    match a {
        PAction::Wifi => system.act(Cmd::ToggleWifi),
        PAction::Bluetooth => system.act(Cmd::ToggleBluetooth),
        PAction::Mute => system.act(Cmd::ToggleMute),
        PAction::Volume | PAction::Brightness => {
            let v = ((x - r.0) / r.2).clamp(0.0, 1.0);
            system.act(if a == PAction::Volume { Cmd::SetVolume(v) } else { Cmd::SetBrightness(v) });
            p.drag = Some((a, r));
        }
        PAction::Settings => {
            open_settings(settings);
            p.closing.get_or_insert_with(Instant::now);
        }
        PAction::LogOut => drop(pancake_ipc::send("quit")),
        PAction::Suspend => system.act(Cmd::Suspend),
        PAction::Reboot => system.act(Cmd::Reboot),
        PAction::PowerOff => system.act(Cmd::PowerOff),
        PAction::PlayPause => system.act(Cmd::PlayPause),
        PAction::Next => system.act(Cmd::Next),
        PAction::Previous => system.act(Cmd::Previous),
        PAction::Close | PAction::Noop => {}
    }
}

/// Open Desktop Settings (or leave it be if it's already open).
fn open_settings(child: &mut Option<std::process::Child>) {
    if child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None))) {
        return;
    }
    let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("pancake-settings"))).filter(|p| p.exists());
    *child = std::process::Command::new(beside.unwrap_or_else(|| "pancake-settings".into())).spawn().ok();
}

fn inside(p: Option<(f64, f64)>, r: Rect) -> bool {
    p.is_some_and(|(x, y)| {
        let (x, y) = (x as f32, y as f32);
        x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
    })
}

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Album art, cropped square and rounded, by (file, pixel size); and
/// blurred backdrops of it, by (file, width, height).
#[derive(Default)]
struct ArtCache(HashMap<(PathBuf, u32), Option<Image>>, HashMap<(PathBuf, u32, u32), Option<Image>>);

impl ArtCache {
    fn get(&mut self, path: &PathBuf, size: u32, radius: f32) -> Option<&Image> {
        self.0
            .entry((path.clone(), size))
            .or_insert_with(|| {
                let img = images::load(path, None)?;
                let px = images::wallpaper(&img, size, size, images::Fit::Fill, 0);
                let mut out = Image { w: size, h: size, px };
                round(&mut out, radius);
                Some(out)
            })
            .as_ref()
    }

    fn backdrop(&mut self, path: &PathBuf, w: u32, h: u32, radius: f32) -> Option<&Image> {
        self.1
            .entry((path.clone(), w, h))
            .or_insert_with(|| {
                let img = images::load(path, None)?;
                let mut out = images::blurred_backdrop(&img, w, h, 0.55);
                round(&mut out, radius);
                Some(out)
            })
            .as_ref()
    }
}

fn round(img: &mut Image, r: f32) {
    let (w, h) = (img.w as f32, img.h as f32);
    for y in 0..img.h {
        for x in 0..img.w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.max(r).min(w - r);
            let cy = fy.max(r).min(h - r);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() - r;
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover < 1.0 {
                let p = &mut img.px[(y * img.w + x) as usize];
                let ch = |s: u32| ((((*p >> s) & 0xff) as f32 * cover) as u32) << s;
                *p = ch(24) | ch(16) | ch(8) | ch(0);
            }
        }
    }
}

/// Everything drawing needs, in one place.
struct Ui<'a, 'b> {
    c: &'a mut Canvas<'b>,
    fonts: &'a mut Fonts,
    icons: &'a mut Icons,
    art: &'a mut ArtCache,
    apps: &'a [pancake_draw::apps::App],
    cfg: &'a Config,
    accent: u32,
    pointer: Option<(f64, f64)>,
}

impl Ui<'_, '_> {
    fn island(&mut self, x: f32, y_mid: f32, w: f32) -> Rect {
        let r = (x, y_mid - ISLAND_H / 2.0, w, ISLAND_H);
        self.c.rounded_rect(r.0, r.1, r.2, r.3, ISLAND_R, ISLAND);
        r
    }

    fn icon_for(&self, app_id: &str) -> Option<String> {
        pancake_draw::apps::for_app_id(self.apps, app_id)
            .and_then(|a| a.icon.clone())
            .or_else(|| (!app_id.is_empty()).then(|| app_id.to_string()))
    }

    /// An app icon centered on y_mid.
    fn app_icon(&mut self, app_id: &str, x: f32, y_mid: f32, size: u32) -> bool {
        let Some(name) = self.icon_for(app_id) else { return false };
        let px = (size as f32 * self.c.scale) as u32;
        match self.icons.get(&name, px) {
            Some(img) => {
                self.c.image(img, x as i32, (y_mid - size as f32 / 2.0) as i32);
                true
            }
            None => false,
        }
    }

    /// A symbolic UI icon, tinted, centered on (cx, cy). Returns false if
    /// the theme doesn't have it.
    fn sym(&mut self, name: &str, cx: f32, cy: f32, size: f32, color: u32) -> bool {
        let px = (size * self.c.scale) as u32;
        match self.icons.symbolic(name, px, color) {
            Some(img) => {
                self.c.image_alpha(&img, cx - size / 2.0, cy - size / 2.0, 1.0);
                true
            }
            None => false,
        }
    }

    fn volume_icon(v: Option<(f32, bool)>) -> &'static str {
        match v {
            Some((_, true)) | Some((0.0, _)) => "audio-volume-muted-symbolic",
            Some((x, _)) if x < 0.34 => "audio-volume-low-symbolic",
            Some((x, _)) if x < 0.67 => "audio-volume-medium-symbolic",
            _ => "audio-volume-high-symbolic",
        }
    }

    fn net_icon(n: &Option<Net>, wifi_on: Option<bool>) -> &'static str {
        match n {
            Some(Net::Ethernet) => "network-wired-symbolic",
            Some(Net::Wifi { signal, .. }) => match signal {
                0..=24 => "network-wireless-signal-weak-symbolic",
                25..=49 => "network-wireless-signal-ok-symbolic",
                50..=74 => "network-wireless-signal-good-symbolic",
                _ => "network-wireless-signal-excellent-symbolic",
            },
            _ if wifi_on == Some(false) => "network-wireless-disabled-symbolic",
            _ => "network-offline-symbolic",
        }
    }

    /// Draw the whole bar; returns what's clickable.
    fn draw_bar(&mut self, status: &pancake_ipc::Status, sys: &stats::Stats, info: &Info, open: Option<PopupKind>) -> Vec<(Rect, Target)> {
        let (w, h) = (self.c.w as f32, self.c.h as f32);
        let accent = 0xff00_0000 | self.accent;
        let on_accent = text_on(self.accent);
        let mid = h / 2.0;
        let base = mid + 5.0;
        let mut hits = Vec::new();
        self.c.clear();
        self.c.rounded_rect(0.0, 0.0, w, h, RADIUS, GLASS_EDGE);
        self.c.rounded_rect(1.0, 1.0, w - 2.0, h - 2.0, RADIUS - 1.0, GLASS);

        // Pancake button.
        let mut x = 6.0;
        let b = (x, mid - ISLAND_H / 2.0, ISLAND_H, ISLAND_H);
        self.c.rounded_rect(b.0, b.1, b.2, b.3, ISLAND_R, if inside(self.pointer, b) { SUBTLE_HOVER } else { ISLAND });
        if let Some(logo) = pancake_draw::logo::image((24.0 * self.c.scale) as u32) {
            self.c.image(&logo, (x + 4.0) as i32, (mid - 12.0) as i32);
        }
        hits.push((b, Target::Launcher));
        x += ISLAND_H + GAP;

        // Workspaces.
        let highest = status.windows.iter().map(|w| w.workspace).chain([status.active_workspace, 5]).max().unwrap_or(5);
        let items: Vec<(u32, Vec<String>)> = (1..=highest)
            .map(|n| (n, status.windows.iter().filter(|w| w.workspace == n).map(|w| w.app_id.clone()).take(3).collect()))
            .collect();
        let item_w = |apps: &Vec<String>| 26.0 + apps.len() as f32 * 19.0;
        let total: f32 = items.iter().map(|(_, a)| item_w(a)).sum::<f32>() + 8.0;
        let isl = self.island(x, mid, total);
        let mut ix = x + 4.0;
        for (n, app_ids) in &items {
            let iw = item_w(app_ids);
            let r = (ix, isl.1 + 3.0, iw, ISLAND_H - 6.0);
            let active = *n == status.active_workspace;
            if active {
                self.c.rounded_rect(r.0, r.1, r.2, r.3, 9.0, accent);
            } else if inside(self.pointer, r) {
                self.c.rounded_rect(r.0, r.1, r.2, r.3, 9.0, SUBTLE_HOVER);
            }
            let fg = if active { on_accent } else if app_ids.is_empty() { TEXT_FAINT } else { TEXT };
            self.c.text(self.fonts, &n.to_string(), ix + 9.0, base - 0.5, 13.0, true, fg);
            let mut ax = ix + 22.0;
            for id in app_ids {
                if !self.app_icon(id, ax, mid, 16) {
                    self.c.circle(ax + 8.0, mid, 3.0, fg);
                }
                ax += 19.0;
            }
            hits.push((r, Target::Workspace(*n)));
            ix += iw;
        }
        x += total + GAP;

        // Clock (centered), with media just left of it.
        let now = Local::now();
        let date = if self.cfg.show_date { now.format("%a %-d %b").to_string() } else { String::new() };
        let time = if self.cfg.clock_24h { now.format("%H:%M") } else { now.format("%-I:%M %p") }.to_string();
        let dgap = if date.is_empty() { 0.0 } else { 10.0 };
        let tw = self.fonts.width(&date, 13.0, false) + dgap + self.fonts.width(&time, 14.0, true);
        let clock_x = (w - tw - 28.0) / 2.0;
        let clock = self.island(clock_x, mid, tw + 28.0);
        if open == Some(PopupKind::Dashboard) || inside(self.pointer, clock) {
            self.c.rounded_rect(clock.0, clock.1, clock.2, clock.3, ISLAND_R, SUBTLE);
        }
        hits.push((clock, Target::Dashboard));
        let dw = self.c.text(self.fonts, &date, clock_x + 14.0, base, 13.0, false, TEXT_DIM);
        self.c.text(self.fonts, &time, clock_x + 14.0 + dw + dgap, base, 14.0, true, TEXT);
        let mut left_limit = clock_x - GAP;

        if let Some(m) = &info.media {
            let label = if m.artist.is_empty() { m.title.clone() } else { format!("{} · {}", m.title, m.artist) };
            // Room left of the clock, keeping some for the focused window.
            let chrome = 8.0 + 22.0 + 8.0 + 8.0 + 24.0 + 6.0;
            let room = clock_x - GAP - x - 90.0 - chrome;
            let label = self.fonts.fit(&label, 13.0, false, room.min(220.0));
            let lw = self.fonts.width(&label, 13.0, false);
            let mw = chrome + lw;
            let mx = clock_x - GAP - mw;
            if room >= 60.0 {
                let isl = self.island(mx, mid, mw);
                if open == Some(PopupKind::Media) || inside(self.pointer, isl) {
                    self.c.rounded_rect(isl.0, isl.1, isl.2, isl.3, ISLAND_R, SUBTLE);
                }
                let art_px = (22.0 * self.c.scale) as u32;
                let drawn = m.art.as_ref().and_then(|p| self.art.get(p, art_px, 6.0 * self.c.scale)).cloned();
                match drawn {
                    Some(img) => self.c.image(&img, (mx + 8.0) as i32, (mid - 11.0) as i32),
                    None => {
                        self.c.rounded_rect(mx + 8.0, mid - 11.0, 22.0, 22.0, 6.0, accent);
                        self.sym("audio-x-generic-symbolic", mx + 19.0, mid, 14.0, on_accent);
                    }
                }
                self.c.text(self.fonts, &label, mx + 38.0, base - 0.5, 13.0, false, TEXT);
                let pb = (mx + mw - 30.0, mid - 12.0, 24.0, 24.0);
                self.c.circle(pb.0 + 12.0, mid, 12.0, if inside(self.pointer, pb) { SUBTLE_HOVER } else { SUBTLE });
                let icon = if m.playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" };
                self.sym(icon, pb.0 + 12.0, mid, 14.0, TEXT);
                hits.push((pb, Target::PlayPause));
                hits.push(((mx, isl.1, mw - 32.0, ISLAND_H), Target::MediaCard));
                left_limit = mx - GAP;
            }
        }

        // Focused window, between the workspaces and the media/clock.
        if let Some(fw) = status.windows.iter().find(|w| w.focused) {
            let title = if fw.title.is_empty() { "Untitled".to_string() } else { fw.title.clone() };
            let max_w = left_limit - x - 24.0 - 24.0 - 8.0;
            if max_w > 60.0 {
                let title = self.fonts.fit(&title, 13.0, false, max_w.min(320.0));
                let tw = self.fonts.width(&title, 13.0, false);
                let px = (18.0 * self.c.scale) as u32;
                let has_icon = self.icon_for(&fw.app_id).is_some_and(|n| self.icons.get(&n, px).is_some());
                let iw = tw + 24.0 + if has_icon { 24.0 } else { 0.0 };
                self.island(x, mid, iw);
                let mut tx = x + 12.0;
                if has_icon && self.app_icon(&fw.app_id.clone(), tx, mid, 18) {
                    tx += 24.0;
                }
                self.c.text(self.fonts, &title, tx, base - 0.5, 13.0, false, TEXT);
            }
        }

        // Right: control center button, status icons, stats.
        let cc = (w - 6.0 - ISLAND_H, mid - ISLAND_H / 2.0, ISLAND_H, ISLAND_H);
        let cc_on = open == Some(PopupKind::Control);
        self.c.rounded_rect(cc.0, cc.1, cc.2, cc.3, ISLAND_R, if cc_on { accent } else if inside(self.pointer, cc) { SUBTLE_HOVER } else { ISLAND });
        let knob_fg = if cc_on { on_accent } else { TEXT };
        let (sx, sy) = (cc.0 + 8.0, mid - 6.0);
        for (i, knob) in [(0.0, 11.0), (6.0, 4.0), (12.0, 9.0)] {
            self.c.rounded_rect(sx, sy + i - 1.0, 16.0, 2.0, 1.0, if cc_on { on_accent } else { TEXT_DIM });
            self.c.circle(sx + knob, sy + i, 2.6, knob_fg);
        }
        hits.push((cc, Target::ControlCenter));
        let mut right = cc.0 - GAP;

        // Status icons: network, Bluetooth (when on), volume with percent.
        let mut icons: Vec<String> = Vec::new();
        if info.net.is_some() || info.wifi_enabled.is_some() {
            icons.push(Self::net_icon(&info.net, info.wifi_enabled).into());
        }
        if info.bluetooth == Some(true) {
            icons.push("bluetooth-active-symbolic".into());
        }
        let vol_label = info.volume.map(|(v, m)| if m { "Muted".to_string() } else { format!("{:.0}%", v * 100.0) });
        let battery = sys.battery.map(|(b, charging)| {
            let level = ((b * 10.0).round() as u32 * 10).min(100);
            (format!("battery-level-{level}{}-symbolic", if charging { "-charging" } else { "" }), format!("{:.0}%", b * 100.0))
        });
        if let Some((name, _)) = &battery {
            icons.push(name.clone());
        }
        if info.volume.is_some() {
            icons.push(Self::volume_icon(info.volume).into());
        }
        if !icons.is_empty() {
            let lw = vol_label.as_ref().map(|l| self.fonts.width(l, 13.0, true) + 4.0).unwrap_or(0.0);
            let sw = 12.0 + icons.len() as f32 * 24.0 + lw + 8.0;
            let sxp = right - sw;
            let isl = self.island(sxp, mid, sw);
            if cc_on || inside(self.pointer, isl) {
                self.c.rounded_rect(isl.0, isl.1, isl.2, isl.3, ISLAND_R, SUBTLE);
            }
            let mut ix = sxp + 10.0;
            for name in &icons {
                self.sym(name, ix + 9.0, mid, 16.0, TEXT);
                ix += 24.0;
            }
            if let Some(l) = &vol_label {
                self.c.text(self.fonts, l, ix, base - 0.5, 13.0, true, TEXT);
            }
            hits.push((isl, Target::ControlCenter));
            right = sxp - GAP;
        }

        let mut parts: Vec<(String, String, f32)> = Vec::new();
        if let Some(c) = sys.cpu {
            parts.push(("CPU".into(), format!("{:.0}%", c * 100.0), c));
        }
        if let Some(m) = sys.mem {
            parts.push(("RAM".into(), format!("{:.0}%", m * 100.0), m));
        }
        if !parts.is_empty() {
            let widths: Vec<f32> = parts.iter().map(|(l, v, _)| self.fonts.width(l, 11.0, true) + 6.0 + self.fonts.width(v, 13.0, true) + 22.0).collect();
            let total: f32 = widths.iter().sum::<f32>() + 8.0;
            let x0 = right - total;
            if x0 > clock_x + tw + 40.0 {
                self.island(x0, mid, total);
                let mut px = x0 + 12.0;
                for ((label, value, fill), pw) in parts.iter().zip(widths) {
                    let lw = self.c.text(self.fonts, label, px, base - 1.0, 11.0, true, TEXT_FAINT);
                    self.c.text(self.fonts, value, px + lw + 6.0, base - 0.5, 13.0, true, TEXT);
                    let bw = pw - 22.0;
                    self.c.rounded_rect(px, mid + 9.0, bw, 2.0, 1.0, 0x22ffffff);
                    self.c.rounded_rect(px, mid + 9.0, (bw * fill.clamp(0.0, 1.0)).max(2.0), 2.0, 1.0, accent);
                    px += pw;
                }
            }
        }
        hits
    }

    /// A slider: track, accent fill, knob. Returns the track rect.
    fn slider(&mut self, x: f32, y: f32, w: f32, v: f32) -> Rect {
        let accent = 0xff00_0000 | self.accent;
        let r = (x, y - 10.0, w, 20.0);
        self.c.rounded_rect(x, y - 3.0, w, 6.0, 3.0, 0x26ffffff);
        let fill = (w * v.clamp(0.0, 1.0)).max(6.0);
        self.c.rounded_rect(x, y - 3.0, fill, 6.0, 3.0, accent);
        self.c.circle(x + fill, y, 8.0, 0xfff6eee3);
        r
    }

    /// Draw the open card. Returns its hit areas and the slider tracks.
    fn draw_card(
        &mut self,
        kind: PopupKind,
        open: f32,
        confirm: Option<PAction>,
        info: &Info,
        sys: &stats::Stats,
        fetch: &[(&'static str, String)],
    ) -> (Vec<(Rect, PAction)>, Option<Vec<(PAction, Rect)>>) {
        let (w, h) = (self.c.w as f32, self.c.h as f32);
        let accent = 0xff00_0000 | self.accent;
        let on_accent = text_on(self.accent);
        let mut hits: Vec<(Rect, PAction)> = Vec::new();
        let mut tracks: Vec<(PAction, Rect)> = Vec::new();
        self.c.clear();
        self.c.rounded_rect(0.0, 0.0, w, h, 0.0, 0x33000000); // light dim
        let card: Rect;

        let top = self.cfg.panel_position == PanelPosition::Top;
        let below_bar = MARGIN as f32 + BAR_HEIGHT as f32 + 10.0;
        let slide = (1.0 - open) * 12.0 * if top { -1.0 } else { 1.0 };
        let pad = 16.0;

        match kind {
            PopupKind::Control => {
                let cw = 380.0;
                // Height: tiles row, sliders, buttons, maybe a confirm line.
                let has_tiles = info.wifi_enabled.is_some() || info.net.is_some() || info.bluetooth.is_some();
                let sliders = info.volume.is_some() as u32 + info.brightness.is_some() as u32;
                let ch = pad + if has_tiles { 72.0 + 14.0 } else { 0.0 } + sliders as f32 * 44.0 + 52.0 + if confirm.is_some() { 26.0 } else { 0.0 } + pad;
                let cx = w - MARGIN as f32 - cw;
                let cy = if top { below_bar } else { h - below_bar - ch } + slide;
                card = (cx, cy, cw, ch);
                self.c.rounded_rect(cx - 1.0, cy - 1.0, cw + 2.0, ch + 2.0, 21.0, GLASS_EDGE);
                self.c.rounded_rect(cx, cy, cw, ch, 20.0, CARD_BG);
                let mut y = cy + pad;

                if has_tiles {
                    let tw = (cw - pad * 2.0 - 10.0) / 2.0;
                    let mut tx = cx + pad;
                    let mut tile = |ui: &mut Self, label: &str, sub: &str, icon: &str, on: bool, action: PAction, hits: &mut Vec<(Rect, PAction)>| {
                        let r = (tx, y, tw, 72.0);
                        let bg = if on { accent } else if inside(ui.pointer, r) { SUBTLE_HOVER } else { SUBTLE };
                        ui.c.rounded_rect(r.0, r.1, r.2, r.3, 18.0, bg);
                        let fg = if on { on_accent } else { TEXT };
                        ui.c.circle(tx + 26.0, y + 36.0, 16.0, if on { 0x33000000 } else { 0x14ffffff });
                        ui.sym(icon, tx + 26.0, y + 36.0, 18.0, fg);
                        ui.c.text(ui.fonts, label, tx + 52.0, y + 32.0, 14.0, true, fg);
                        let sub = ui.fonts.fit(sub, 12.0, false, tw - 60.0);
                        ui.c.text(ui.fonts, &sub, tx + 52.0, y + 50.0, 12.0, false, if on { on_accent } else { TEXT_DIM });
                        hits.push((r, action));
                        tx += tw + 10.0;
                    };
                    if info.wifi_enabled.is_some() || info.net.is_some() {
                        let (label, sub, on) = match (&info.net, info.wifi_enabled) {
                            (Some(Net::Wifi { ssid, .. }), _) => ("Wi-Fi", ssid.clone(), true),
                            (Some(Net::Ethernet), w) => ("Ethernet", if w == Some(true) { "Connected · Wi-Fi on".into() } else { "Connected".into() }, true),
                            (_, Some(true)) => ("Wi-Fi", "On · not connected".to_string(), true),
                            _ => ("Wi-Fi", "Off".to_string(), false),
                        };
                        let icon = Self::net_icon(&info.net, info.wifi_enabled);
                        tile(self, label, &sub, icon, on, PAction::Wifi, &mut hits);
                    }
                    if let Some(bt) = info.bluetooth {
                        let icon = if bt { "bluetooth-active-symbolic" } else { "bluetooth-disabled-symbolic" };
                        tile(self, "Bluetooth", if bt { "On" } else { "Off" }, icon, bt, PAction::Bluetooth, &mut hits);
                    }
                    y += 72.0 + 14.0;
                }

                let mut slider_row = |ui: &mut Self, icon: &str, v: f32, label: String, act: PAction, icon_act: Option<PAction>, hits: &mut Vec<(Rect, PAction)>, tracks: &mut Vec<(PAction, Rect)>| {
                    let ib = (cx + pad, y + 4.0, 36.0, 36.0);
                    ui.c.circle(ib.0 + 18.0, ib.1 + 18.0, 18.0, if icon_act.is_some() && inside(ui.pointer, ib) { SUBTLE_HOVER } else { SUBTLE });
                    ui.sym(icon, ib.0 + 18.0, ib.1 + 18.0, 18.0, TEXT);
                    if let Some(a) = icon_act {
                        hits.push((ib, a));
                    }
                    let tr = ui.slider(cx + pad + 52.0, y + 22.0, cw - pad * 2.0 - 52.0 - 52.0, v);
                    ui.c.text(ui.fonts, &label, cx + cw - pad - 42.0, y + 27.0, 13.0, true, TEXT);
                    hits.push((tr, act));
                    tracks.push((act, tr));
                    y += 44.0;
                };
                if let Some((v, muted)) = info.volume {
                    let label = if muted { "Mute".to_string() } else { format!("{:.0}%", v * 100.0) };
                    slider_row(self, Self::volume_icon(info.volume), v, label, PAction::Volume, Some(PAction::Mute), &mut hits, &mut tracks);
                }
                if let Some(b) = info.brightness {
                    slider_row(self, "display-brightness-symbolic", b, format!("{:.0}%", b * 100.0), PAction::Brightness, None, &mut hits, &mut tracks);
                }

                // Buttons: Settings on the left; log out / suspend / restart /
                // power off on the right (each asks for a second click).
                y += 8.0;
                let sb = (cx + pad, y, 120.0, 40.0);
                self.c.rounded_rect(sb.0, sb.1, sb.2, sb.3, 14.0, if inside(self.pointer, sb) { SUBTLE_HOVER } else { SUBTLE });
                self.sym("emblem-system-symbolic", sb.0 + 22.0, sb.1 + 20.0, 16.0, TEXT);
                self.c.text(self.fonts, "Settings", sb.0 + 38.0, sb.1 + 25.0, 13.0, true, TEXT);
                hits.push((sb, PAction::Settings));
                let mut bx = cx + cw - pad - 40.0;
                let mut buttons = vec![(PAction::PowerOff, "system-shutdown-symbolic", "power off"), (PAction::Reboot, "system-reboot-symbolic", "restart")];
                if info.can_power {
                    buttons.push((PAction::Suspend, "weather-clear-night-symbolic", "suspend"));
                } else {
                    buttons.clear();
                }
                buttons.push((PAction::LogOut, "system-log-out-symbolic", "log out"));
                let mut confirm_text = None;
                for (a, icon, name) in buttons {
                    let r = (bx, y, 40.0, 40.0);
                    let asking = confirm == Some(a);
                    let bg = if asking { 0xffbc4b51 } else if inside(self.pointer, r) { SUBTLE_HOVER } else { SUBTLE };
                    self.c.circle(bx + 20.0, y + 20.0, 20.0, bg);
                    self.sym(icon, bx + 20.0, y + 20.0, 16.0, TEXT);
                    hits.push((r, a));
                    if asking {
                        confirm_text = Some(format!("Click again to {name}"));
                    }
                    bx -= 46.0;
                }
                if let Some(t) = confirm_text {
                    let tw = self.fonts.width(&t, 12.0, true);
                    self.c.text(self.fonts, &t, cx + cw - pad - tw, y + 62.0, 12.0, true, 0xffe8848a);
                }
            }
            PopupKind::Dashboard => {
                card = self.draw_dashboard(w, h, top, below_bar, slide, info, sys, fetch, &mut hits);
            }
            PopupKind::Media => {
                let cw = 420.0;
                let ch = 176.0;
                let cx = (w - cw) / 2.0 - 60.0;
                let cy = if top { below_bar } else { h - below_bar - ch } + slide;
                card = (cx, cy, cw, ch);
                self.c.rounded_rect(cx - 1.0, cy - 1.0, cw + 2.0, ch + 2.0, 21.0, GLASS_EDGE);
                self.c.rounded_rect(cx, cy, cw, ch, 20.0, CARD_BG);
                match &info.media {
                    None => {
                        let t = "Nothing playing";
                        let tw = self.fonts.width(t, 15.0, true);
                        self.c.text(self.fonts, t, cx + (cw - tw) / 2.0, cy + ch / 2.0 + 5.0, 15.0, true, TEXT_DIM);
                    }
                    Some(m) => {
                        let art_px = (112.0 * self.c.scale) as u32;
                        let art = m.art.as_ref().and_then(|p| self.art.get(p, art_px, 16.0 * self.c.scale)).cloned();
                        match art {
                            Some(img) => self.c.image(&img, (cx + pad) as i32, (cy + pad) as i32),
                            None => {
                                self.c.rounded_rect(cx + pad, cy + pad, 112.0, 112.0, 16.0, accent);
                                self.sym("audio-x-generic-symbolic", cx + pad + 56.0, cy + pad + 56.0, 48.0, on_accent);
                            }
                        }
                        let tx = cx + pad + 112.0 + 16.0;
                        let tw = cw - (tx - cx) - pad;
                        let title = self.fonts.fit(if m.title.is_empty() { "Unknown" } else { &m.title }, 16.0, true, tw);
                        self.c.text(self.fonts, &title, tx, cy + pad + 18.0, 16.0, true, TEXT);
                        let artist = self.fonts.fit(&m.artist, 13.0, false, tw);
                        self.c.text(self.fonts, &artist, tx, cy + pad + 40.0, 13.0, false, TEXT_DIM);
                        let player = self.fonts.fit(&m.player, 11.0, false, tw);
                        self.c.text(self.fonts, &player, tx, cy + pad + 58.0, 11.0, false, TEXT_FAINT);
                        // Controls.
                        let row = cy + pad + 92.0;
                        let mid_x = tx + tw / 2.0;
                        for (dx, action, icon, big) in [
                            (-54.0, PAction::Previous, "media-skip-backward-symbolic", false),
                            (0.0, PAction::PlayPause, if m.playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }, true),
                            (54.0, PAction::Next, "media-skip-forward-symbolic", false),
                        ] {
                            let rad = if big { 22.0 } else { 17.0 };
                            let r = (mid_x + dx - rad, row - rad, rad * 2.0, rad * 2.0);
                            let bg = if big { accent } else if inside(self.pointer, r) { SUBTLE_HOVER } else { SUBTLE };
                            self.c.circle(mid_x + dx, row, rad, bg);
                            self.sym(icon, mid_x + dx, row, if big { 20.0 } else { 16.0 }, if big { on_accent } else { TEXT });
                            hits.push((r, action));
                        }
                        // Progress.
                        let py = cy + ch - 22.0;
                        let pos = m.position_now();
                        let frac = if m.length > 0.0 { (pos / m.length) as f32 } else { 0.0 };
                        let (bx, bw) = (cx + pad + 44.0, cw - pad * 2.0 - 88.0);
                        self.c.rounded_rect(bx, py - 2.0, bw, 4.0, 2.0, 0x26ffffff);
                        self.c.rounded_rect(bx, py - 2.0, (bw * frac.clamp(0.0, 1.0)).max(4.0), 4.0, 2.0, accent);
                        self.c.text(self.fonts, &mmss(pos), cx + pad, py + 4.0, 11.0, true, TEXT_DIM);
                        if m.length > 0.0 {
                            let l = mmss(m.length);
                            let lw = self.fonts.width(&l, 11.0, true);
                            self.c.text(self.fonts, &l, cx + cw - pad - lw, py + 4.0, 11.0, true, TEXT_DIM);
                        }
                    }
                }
            }
        }
        // Topmost first: controls, then the card itself (no-op), then the
        // backdrop around it (closes).
        hits.push((card, PAction::Noop));
        hits.push(((0.0, 0.0, w, h), PAction::Close));
        self.c.fade_all(open);
        (hits, Some(tracks))
    }
}

impl Ui<'_, '_> {
    fn tile(&mut self, r: Rect) {
        self.c.rounded_rect(r.0, r.1, r.2, r.3, 16.0, 0x12ffffff);
    }

    fn weather_icon(condition: &str) -> &'static str {
        let c = condition.to_lowercase();
        let night = !(6..19).contains(&chrono::Timelike::hour(&Local::now()));
        if c.contains("thunder") {
            "weather-storm-symbolic"
        } else if c.contains("snow") || c.contains("sleet") || c.contains("ice") {
            "weather-snow-symbolic"
        } else if c.contains("rain") || c.contains("drizzle") || c.contains("shower") {
            "weather-showers-symbolic"
        } else if c.contains("fog") || c.contains("mist") || c.contains("haze") {
            "weather-fog-symbolic"
        } else if c.contains("overcast") {
            "weather-overcast-symbolic"
        } else if c.contains("cloud") {
            if night { "weather-few-clouds-night-symbolic" } else { "weather-few-clouds-symbolic" }
        } else if night {
            "weather-clear-night-symbolic"
        } else {
            "weather-clear-symbolic"
        }
    }

    /// A gauge ring: track and accent value, value text in the middle.
    fn gauge(&mut self, cx: f32, cy: f32, v: Option<f32>, value: &str, label: &str) {
        let accent = 0xff00_0000 | self.accent;
        self.c.arc(cx, cy, 26.0, 5.0, 220.0, 280.0, 0x22ffffff);
        if let Some(v) = v {
            self.c.arc(cx, cy, 26.0, 5.0, 220.0, 280.0 * v.clamp(0.0, 1.0), accent);
        }
        let vw = self.fonts.width(value, 13.0, true);
        self.c.text(self.fonts, value, cx - vw / 2.0, cy + 4.0, 13.0, true, TEXT);
        let lw = self.fonts.width(label, 10.0, true);
        self.c.text(self.fonts, label, cx - lw / 2.0, cy + 30.0, 10.0, true, TEXT_FAINT);
    }

    /// The dashboard (click the clock): weather, clock, system info,
    /// calendar, performance gauges and media. Returns the card rect.
    #[allow(clippy::too_many_arguments)]
    fn draw_dashboard(
        &mut self,
        w: f32,
        h: f32,
        top: bool,
        below_bar: f32,
        slide: f32,
        info: &Info,
        sys: &stats::Stats,
        fetch: &[(&'static str, String)],
        hits: &mut Vec<(Rect, PAction)>,
    ) -> Rect {
        let accent = 0xff00_0000 | self.accent;
        let on_accent = text_on(self.accent);
        let (pad, gap) = (14.0, 10.0);
        let (wa, wb, wc, wd) = (230.0, 300.0, 170.0, 200.0);
        let show_gauges = w > wa + wb + wc + wd + gap * 3.0 + pad * 2.0 + 40.0;
        let inner_w = wa + wb + wd + gap * 2.0 + if show_gauges { wc + gap } else { 0.0 };
        let (cw, ch) = (inner_w + pad * 2.0, 298.0 + pad * 2.0);
        let cx = ((w - cw) / 2.0).max(MARGIN as f32);
        let cy = if top { below_bar } else { h - below_bar - ch } + slide;
        self.c.rounded_rect(cx - 1.0, cy - 1.0, cw + 2.0, ch + 2.0, 25.0, GLASS_EDGE);
        self.c.rounded_rect(cx, cy, cw, ch, 24.0, CARD_BG);
        let (x0, y0) = (cx + pad, cy + pad);
        let now = Local::now();

        // Weather.
        let r = (x0, y0, wa, 118.0);
        self.tile(r);
        match &info.weather {
            Some(wt) => {
                self.sym(Self::weather_icon(&wt.condition), r.0 + 46.0, r.1 + 59.0, 52.0, TEXT);
                self.c.text(self.fonts, &wt.temp, r.0 + 88.0, r.1 + 52.0, 28.0, true, TEXT);
                let cond = self.fonts.fit(&wt.condition, 13.0, false, wa - 100.0);
                self.c.text(self.fonts, &cond, r.0 + 88.0, r.1 + 74.0, 13.0, false, TEXT_DIM);
                let place = self.fonts.fit(&format!("{} · {}", wt.location, wt.humidity), 11.0, false, wa - 100.0);
                self.c.text(self.fonts, &place, r.0 + 88.0, r.1 + 92.0, 11.0, false, TEXT_FAINT);
            }
            None => {
                self.sym("weather-clear-symbolic", r.0 + 46.0, r.1 + 59.0, 44.0, TEXT_FAINT);
                let msg = if self.cfg.weather { "Getting weather…" } else { "Weather is off" };
                self.c.text(self.fonts, msg, r.0 + 88.0, r.1 + 64.0, 13.0, false, TEXT_DIM);
            }
        }

        // Big clock and date.
        let r = (x0, y0 + 128.0, wa, 170.0);
        self.tile(r);
        let time = if self.cfg.clock_24h { now.format("%H:%M") } else { now.format("%-I:%M") }.to_string();
        let tw = self.fonts.width(&time, 50.0, true);
        self.c.text(self.fonts, &time, r.0 + (wa - tw) / 2.0, r.1 + 82.0, 50.0, true, TEXT);
        let date = now.format("%A, %-d %B").to_string();
        let date = self.fonts.fit(&date, 14.0, true, wa - 24.0);
        let dw = self.fonts.width(&date, 14.0, true);
        self.c.text(self.fonts, &date, r.0 + (wa - dw) / 2.0, r.1 + 112.0, 14.0, true, accent);
        if let Some(up) = pancake_fetch::uptime_secs() {
            let t = format!("up {}", pancake_fetch::format_uptime(up));
            let t = self.fonts.fit(&t, 11.0, false, wa - 24.0);
            let uw = self.fonts.width(&t, 11.0, false);
            self.c.text(self.fonts, &t, r.0 + (wa - uw) / 2.0, r.1 + 140.0, 11.0, false, TEXT_FAINT);
        }

        // System info (fetch-style) with the palette as dots.
        let xb = x0 + wa + gap;
        let r = (xb, y0, wb, 118.0);
        self.tile(r);
        if let Some(logo) = pancake_draw::logo::image((52.0 * self.c.scale) as u32) {
            self.c.image(&logo, (r.0 + 12.0) as i32, (r.1 + 14.0) as i32);
        }
        let mut ly = r.1 + 24.0;
        for key in ["OS", "Kernel", "Packages", "Shell"] {
            if let Some((_, v)) = fetch.iter().find(|(k, _)| *k == key) {
                let v = if key == "Packages" { v.split(", ").next().unwrap_or(v).to_string() } else { v.clone() };
                let kw = self.c.text(self.fonts, key, r.0 + 76.0, ly, 11.0, true, accent);
                let v = self.fonts.fit(&v, 11.0, false, wb - 84.0 - kw - 6.0);
                self.c.text(self.fonts, &v, r.0 + 76.0 + kw + 6.0, ly, 11.0, false, TEXT);
                ly += 17.0;
            }
        }
        for (i, col) in ACCENTS.iter().enumerate() {
            self.c.circle(r.0 + 22.0 + i as f32 * 20.0, r.1 + 100.0, 6.0, *col);
        }

        // Calendar for this month, today highlighted.
        let r = (xb, y0 + 128.0, wb, 170.0);
        self.tile(r);
        let month = now.format("%B %Y").to_string();
        self.c.text(self.fonts, &month, r.0 + 14.0, r.1 + 22.0, 13.0, true, TEXT);
        let today = now.date_naive();
        let first = chrono::NaiveDate::from_ymd_opt(chrono::Datelike::year(&today), chrono::Datelike::month(&today), 1).unwrap();
        let lead = chrono::Datelike::weekday(&first).num_days_from_sunday() as i32;
        let days_in = {
            let (y, m) = (chrono::Datelike::year(&today), chrono::Datelike::month(&today));
            let next = if m == 12 { chrono::NaiveDate::from_ymd_opt(y + 1, 1, 1) } else { chrono::NaiveDate::from_ymd_opt(y, m + 1, 1) }.unwrap();
            (next - first).num_days() as i32
        };
        let cell_w = (wb - 24.0) / 7.0;
        for (i, d) in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().enumerate() {
            let dw = self.fonts.width(d, 10.0, true);
            self.c.text(self.fonts, d, r.0 + 12.0 + i as f32 * cell_w + (cell_w - dw) / 2.0, r.1 + 42.0, 10.0, true, TEXT_FAINT);
        }
        let day_today = chrono::Datelike::day(&today) as i32;
        for d in 1..=days_in {
            let slot = lead + d - 1;
            let (col, row) = (slot % 7, slot / 7);
            let (dx, dy) = (r.0 + 12.0 + col as f32 * cell_w, r.1 + 52.0 + row as f32 * 19.0);
            let label = d.to_string();
            let lw = self.fonts.width(&label, 11.0, d == day_today);
            if d == day_today {
                self.c.circle(dx + cell_w / 2.0, dy + 8.0, 9.5, accent);
            }
            let fg = if d == day_today { on_accent } else if col == 0 || col == 6 { TEXT_DIM } else { TEXT };
            self.c.text(self.fonts, &label, dx + (cell_w - lw) / 2.0, dy + 12.0, 11.0, d == day_today, fg);
        }

        // Performance gauges.
        let mut xd = xb + wb + gap;
        if show_gauges {
            let r = (xd, y0, wc, 298.0);
            self.tile(r);
            let pct = |v: Option<f32>| v.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "–".into());
            let temp = sys.temp.map(|t| format!("{t:.0}°")).unwrap_or_else(|| "–".into());
            let (lx, rx) = (r.0 + wc / 4.0 + 2.0, r.0 + wc * 3.0 / 4.0 - 2.0);
            self.gauge(lx, r.1 + 78.0, sys.cpu, &pct(sys.cpu), "CPU");
            self.gauge(rx, r.1 + 78.0, sys.temp.map(|t| t / 100.0), &temp, "TEMP");
            self.gauge(lx, r.1 + 206.0, sys.mem, &pct(sys.mem), "MEMORY");
            self.gauge(rx, r.1 + 206.0, sys.disk, &pct(sys.disk), "DISK");
            xd += wc + gap;
        }

        // Media: the cover art, crisp, over a blurred and dimmed copy of
        // itself filling the tile.
        let r = (xd, y0, wd, 298.0);
        let s = self.c.scale;
        let backdrop = info.media.as_ref().and_then(|m| m.art.as_ref()).and_then(|p| self.art.backdrop(p, (wd * s) as u32, (298.0 * s) as u32, 16.0 * s)).cloned();
        match backdrop {
            Some(bg) => self.c.image(&bg, r.0 as i32, r.1 as i32),
            None => self.tile(r),
        }
        let vx = r.0 + wd / 2.0;
        match &info.media {
            Some(m) => {
                let (ax, ay, asz) = (vx - 64.0, r.1 + 18.0, 128.0);
                // A soft shadow under the cover.
                self.c.rounded_rect(ax + 2.0, ay + 6.0, asz, asz, 14.0, 0x40000000);
                self.c.rounded_rect(ax - 1.0, ay + 2.0, asz + 2.0, asz + 2.0, 15.0, 0x26000000);
                let art_px = (asz * s) as u32;
                let art = m.art.as_ref().and_then(|p| self.art.get(p, art_px, 14.0 * s)).cloned();
                match art {
                    Some(img) => self.c.image(&img, ax as i32, ay as i32),
                    None => {
                        self.c.rounded_rect(ax, ay, asz, asz, 14.0, accent);
                        self.sym("audio-x-generic-symbolic", vx, ay + asz / 2.0, 44.0, on_accent);
                    }
                }
                let title = self.fonts.fit(if m.title.is_empty() { "Unknown" } else { &m.title }, 14.0, true, wd - 24.0);
                let tw = self.fonts.width(&title, 14.0, true);
                self.c.text(self.fonts, &title, vx - tw / 2.0, r.1 + 172.0, 14.0, true, TEXT);
                let artist = self.fonts.fit(&m.artist, 12.0, false, wd - 24.0);
                let aw = self.fonts.width(&artist, 12.0, false);
                self.c.text(self.fonts, &artist, vx - aw / 2.0, r.1 + 190.0, 12.0, false, TEXT_DIM);
                // Thin progress bar with times.
                let pos = m.position_now();
                let frac = if m.length > 0.0 { (pos / m.length) as f32 } else { 0.0 };
                let (bx, bw, by) = (r.0 + 16.0, wd - 32.0, r.1 + 210.0);
                self.c.rounded_rect(bx, by, bw, 3.0, 1.5, 0x33ffffff);
                self.c.rounded_rect(bx, by, (bw * frac.clamp(0.0, 1.0)).max(3.0), 3.0, 1.5, 0xfff6eee3);
                self.c.text(self.fonts, &mmss(pos), bx, by + 16.0, 10.0, true, TEXT_DIM);
                if m.length > 0.0 {
                    let l = mmss(m.length);
                    let lw = self.fonts.width(&l, 10.0, true);
                    self.c.text(self.fonts, &l, bx + bw - lw, by + 16.0, 10.0, true, TEXT_DIM);
                }
                let row = r.1 + 260.0;
                for (dx, action, icon, big) in [
                    (-48.0, PAction::Previous, "media-skip-backward-symbolic", false),
                    (0.0, PAction::PlayPause, if m.playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }, true),
                    (48.0, PAction::Next, "media-skip-forward-symbolic", false),
                ] {
                    let rad = if big { 20.0 } else { 15.0 };
                    let br = (vx + dx - rad, row - rad, rad * 2.0, rad * 2.0);
                    let bg = if big { accent } else if inside(self.pointer, br) { SUBTLE_HOVER } else { SUBTLE };
                    self.c.circle(vx + dx, row, rad, bg);
                    self.sym(icon, vx + dx, row, if big { 18.0 } else { 14.0 }, if big { on_accent } else { TEXT });
                    hits.push((br, action));
                }
            }
            None => {
                let vy = r.1 + 110.0;
                self.c.rounded_rect(vx - 48.0, vy - 48.0, 96.0, 96.0, 14.0, 0x1affffff);
                self.sym("audio-x-generic-symbolic", vx, vy, 36.0, TEXT_FAINT);
                let t = "Nothing playing";
                let tw = self.fonts.width(t, 13.0, true);
                self.c.text(self.fonts, t, vx - tw / 2.0, r.1 + 200.0, 13.0, true, TEXT_DIM);
            }
        }
        (cx, cy, cw, ch)
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, usize> for App {
    fn event(app: &mut Self, layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, owner: &usize, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                let h = if height == 0 && *owner == BAR { BAR_HEIGHT } else { height.max(1) };
                app.size[*owner] = (width.max(1), h);
                app.configured[*owner] = true;
            }
            zwlr_layer_surface_v1::Event::Closed if *owner == BAR => app.running = false,
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, (usize, usize)> for App {
    fn event(app: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, (owner, i): &(usize, usize), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            app.buffer_free[*owner][*i] = true;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(_: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(app: &mut Self, _: &wl_pointer::WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { surface, surface_x, surface_y, .. } => {
                app.pointer_on = if app.surface_ids[POPUP].as_ref() == Some(&surface.id()) { POPUP } else { BAR };
                app.pointer = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => app.pointer = Some((surface_x, surface_y)),
            wl_pointer::Event::Leave { .. } => app.pointer = None,
            wl_pointer::Event::Button { state: WEnum::Value(s), .. } => {
                app.button_down = s == wl_pointer::ButtonState::Pressed;
                if app.button_down {
                    app.clicked = app.pointer.map(|(x, y)| (app.pointer_on, x, y));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
