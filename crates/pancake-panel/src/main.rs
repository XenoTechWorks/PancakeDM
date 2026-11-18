//! pancake-panel: the PancakeDM top bar.
//!
//! A floating, rounded bar (wlr-layer-shell) made of "widget islands":
//! the Pancake button and a workspace switcher with each workspace's app
//! icons, the focused window, the clock, system stats, and the settings
//! button. Window and workspace info comes from the compositor's control
//! socket; position, clock style and accent (from the wallpaper, if auto)
//! come from pancake.conf and update live. Drawn by hand, no toolkit.

mod stats;

use chrono::Local;
use memmap2::MmapMut;
use pancake_config::{Config, PanelPosition};
use pancake_draw::color::*;
use pancake_draw::icons::Icons;
use pancake_draw::{Canvas, Fonts};
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

const BAR_HEIGHT: u32 = 44;
const MARGIN: i32 = 8;
const RADIUS: f32 = 16.0;
const ISLAND_H: f32 = 32.0;
const ISLAND_R: f32 = 12.0;
const ISLAND: u32 = 0x14ffffff;
const GAP: f32 = 6.0;

struct App {
    configured: bool,
    size: (u32, u32),
    running: bool,
    pointer: Option<(f64, f64)>,
    clicked: Option<(f64, f64)>,
    buffer_free: [bool; 2],
}

type Rect = (f32, f32, f32, f32);

/// Clickable things drawn this frame.
struct Hit {
    rect: Rect,
    target: Target,
}

enum Target {
    Launcher,
    Workspace(u32),
    Settings,
}

/// Anchor and margins for the configured position.
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
    let layer = layer_shell.get_layer_surface(&surface, None, zwlr_layer_shell_v1::Layer::Top, "panel".into(), &qh, ());
    let mut cfg = pancake_config::load();
    let mut accent = pancake_draw::accent::effective(&cfg);
    let mut watch = pancake_config::Watcher::new();
    place(&layer, cfg.panel_position);
    layer.set_size(0, BAR_HEIGHT);
    layer.set_exclusive_zone(BAR_HEIGHT as i32);
    surface.commit();
    let apps = pancake_draw::apps::load_all();
    let mut icons = Icons::new(&cfg.icon_theme);
    let mut settings: Option<std::process::Child> = None;
    let mut sys = stats::Stats::new();

    let mut app = App { configured: false, size: (0, BAR_HEIGHT), running: true, pointer: None, clicked: None, buffer_free: [true; 2] };
    let mut bufs: Vec<(wl_buffer::WlBuffer, MmapMut, (u32, u32))> = Vec::new();
    let mut status = pancake_ipc::Status::default();
    let mut hits: Vec<Hit> = Vec::new();
    let mut last_poll = Instant::now() - Duration::from_secs(10);
    let mut last_stats = Instant::now() - Duration::from_secs(10);
    let mut last_drawn = String::new();
    let fd = conn.backend().poll_fd().as_raw_fd();

    while app.running {
        if let Some((x, y)) = app.clicked.take() {
            let (x, y) = (x as f32, y as f32);
            if let Some(h) = hits.iter().find(|h| x >= h.rect.0 && x < h.rect.0 + h.rect.2 && y >= h.rect.1 && y < h.rect.1 + h.rect.3) {
                match h.target {
                    Target::Launcher => drop(pancake_ipc::send("launcher")),
                    Target::Workspace(n) => drop(pancake_ipc::send(&format!("action workspace {n}"))),
                    Target::Settings => open_settings(&mut settings),
                }
                last_poll = Instant::now() - Duration::from_secs(10);
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
                cfg = new;
                last_drawn.clear();
            }
        }
        if last_stats.elapsed() >= Duration::from_secs(2) {
            sys.refresh();
            last_stats = Instant::now();
        }

        // Redraw only when something visible changed.
        let now = Local::now();
        let key = format!(
            "{:?}{:?}{}{:?}{:?}{}{:?}",
            app.size,
            status,
            now.format("%H:%M"),
            app.pointer.map(|(x, y)| (x as i32 / 4, y as i32 / 4)),
            cfg,
            accent,
            sys.summary()
        );
        if app.configured && key != last_drawn {
            let (w, h) = app.size;
            if bufs.first().map_or(true, |b| b.2 != (w, h)) {
                for b in bufs.drain(..) {
                    b.0.destroy();
                }
                bufs = (0..2).map(|i| make_buffer(&shm, &qh, w, h, i)).collect::<Result<_, _>>()?;
                app.buffer_free = [true; 2];
            }
            if let Some(i) = (0..2).find(|&i| app.buffer_free[i]) {
                let px = pancake_draw::pixels(&mut bufs[i].1);
                let mut canvas = Canvas { px, w: w as i32, h: h as i32 };
                let mut ui = Ui { c: &mut canvas, fonts: &mut fonts, icons: &mut icons, apps: &apps, cfg: &cfg, accent, pointer: app.pointer };
                hits = ui.draw_bar(&status, &sys);
                surface.attach(Some(&bufs[i].0), 0, 0);
                surface.damage_buffer(0, 0, w as i32, h as i32);
                surface.commit();
                app.buffer_free[i] = false;
                last_drawn = key;
            }
        }

        conn.flush().map_err(|e| e.to_string())?;
        if let Some(guard) = queue.prepare_read() {
            if pancake_draw::wait_readable(fd, 250) {
                guard.read().map_err(|e| e.to_string())?;
            }
        }
        queue.dispatch_pending(&mut app).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn make_buffer(shm: &wl_shm::WlShm, qh: &QueueHandle<App>, w: u32, h: u32, i: usize) -> Result<(wl_buffer::WlBuffer, MmapMut, (u32, u32)), String> {
    let size = (w * h * 4) as u64;
    let (file, mem) = pancake_draw::shm_file(size, &format!("panel-{i}"))?;
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888, qh, i);
    pool.destroy();
    Ok((buffer, mem, (w, h)))
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

/// Everything drawing needs, in one place.
struct Ui<'a, 'b> {
    c: &'a mut Canvas<'b>,
    fonts: &'a mut Fonts,
    icons: &'a mut Icons,
    apps: &'a [pancake_draw::apps::App],
    cfg: &'a Config,
    accent: u32,
    pointer: Option<(f64, f64)>,
}

impl Ui<'_, '_> {
    fn mid(&self) -> f32 {
        self.c.h as f32 / 2.0
    }

    fn island(&mut self, x: f32, w: f32) -> Rect {
        let r = (x, self.mid() - ISLAND_H / 2.0, w, ISLAND_H);
        self.c.rounded_rect(r.0, r.1, r.2, r.3, ISLAND_R, ISLAND);
        r
    }

    fn icon_for(&self, app_id: &str) -> Option<String> {
        pancake_draw::apps::for_app_id(self.apps, app_id)
            .and_then(|a| a.icon.clone())
            .or_else(|| (!app_id.is_empty()).then(|| app_id.to_string()))
    }

    fn draw_icon(&mut self, app_id: &str, x: f32, size: u32) -> bool {
        let Some(name) = self.icon_for(app_id) else { return false };
        let mid = self.mid();
        match self.icons.get(&name, size) {
            Some(img) => {
                self.c.image(img, x as i32, (mid - size as f32 / 2.0) as i32);
                true
            }
            None => false,
        }
    }

    /// Draw the whole bar; returns what's clickable.
    fn draw_bar(&mut self, status: &pancake_ipc::Status, sys: &stats::Stats) -> Vec<Hit> {
        let (w, h) = (self.c.w as f32, self.c.h as f32);
        let accent = 0xff00_0000 | self.accent;
        let on_accent = text_on(self.accent);
        let mid = self.mid();
        let base = mid + 5.0;
        let mut hits = Vec::new();
        self.c.clear();
        self.c.rounded_rect(0.0, 0.0, w, h, RADIUS, GLASS_EDGE);
        self.c.rounded_rect(1.0, 1.0, w - 2.0, h - 2.0, RADIUS - 1.0, GLASS);

        // Pancake button: a round island with the logo.
        let mut x = 6.0;
        let b = (x, mid - ISLAND_H / 2.0, ISLAND_H, ISLAND_H);
        self.c.rounded_rect(b.0, b.1, b.2, b.3, ISLAND_R, if inside(self.pointer, b) { SUBTLE_HOVER } else { ISLAND });
        self.c.circle(x + 16.0, mid + 1.0, 9.0, GOLD_DARK);
        self.c.circle(x + 16.0, mid, 7.5, GOLD);
        self.c.circle(x + 13.5, mid - 2.5, 2.0, 0x55ffffff);
        hits.push(Hit { rect: b, target: Target::Launcher });
        x += ISLAND_H + GAP;

        // Workspaces: 1-5 always, more if in use. Active = accent pill.
        let highest = status.windows.iter().map(|w| w.workspace).chain([status.active_workspace, 5]).max().unwrap_or(5);
        let items: Vec<(u32, Vec<String>)> = (1..=highest)
            .map(|n| (n, status.windows.iter().filter(|w| w.workspace == n).map(|w| w.app_id.clone()).take(3).collect()))
            .collect();
        let item_w = |apps: &Vec<String>| 26.0 + apps.len() as f32 * 19.0;
        let total: f32 = items.iter().map(|(_, a)| item_w(a)).sum::<f32>() + 8.0;
        let isl = self.island(x, total);
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
            let label = n.to_string();
            self.c.text(self.fonts, &label, ix + 9.0, base - 0.5, 13.0, true, fg);
            let mut ax = ix + 22.0;
            for id in app_ids {
                if !self.draw_icon(id, ax, 16) {
                    // No icon found: a small dot keeps the count readable.
                    self.c.circle(ax + 8.0, mid, 3.0, fg);
                }
                ax += 19.0;
            }
            hits.push(Hit { rect: r, target: Target::Workspace(*n) });
            ix += iw;
        }
        x += total + GAP;

        // Focused window: icon and title.
        if let Some(fw) = status.windows.iter().find(|w| w.focused) {
            let title = if fw.title.is_empty() { "Untitled".to_string() } else { fw.title.clone() };
            let max_w = (w / 2.0 - 130.0 - x - 40.0).max(60.0);
            let title = self.fonts.fit(&title, 13.0, false, max_w);
            let tw = self.fonts.width(&title, 13.0, false);
            let has_icon = self.icon_for(&fw.app_id).is_some_and(|n| self.icons.get(&n, 18).is_some());
            let iw = tw + 24.0 + if has_icon { 24.0 } else { 0.0 };
            self.island(x, iw);
            let mut tx = x + 12.0;
            if has_icon && self.draw_icon(&fw.app_id.clone(), tx, 18) {
                tx += 24.0;
            }
            self.c.text(self.fonts, &title, tx, base - 0.5, 13.0, false, TEXT);
        }

        // Clock island, centered.
        let now = Local::now();
        let date = if self.cfg.show_date { now.format("%a %-d %b").to_string() } else { String::new() };
        let time = if self.cfg.clock_24h { now.format("%H:%M") } else { now.format("%-I:%M %p") }.to_string();
        let gap = if date.is_empty() { 0.0 } else { 10.0 };
        let tw = self.fonts.width(&date, 13.0, false) + gap + self.fonts.width(&time, 14.0, true);
        let cx = (w - tw - 28.0) / 2.0;
        self.island(cx, tw + 28.0);
        let dw = self.c.text(self.fonts, &date, cx + 14.0, base, 13.0, false, TEXT_DIM);
        self.c.text(self.fonts, &time, cx + 14.0 + dw + gap, base, 14.0, true, TEXT);

        // Right: settings button, then system stats to its left.
        let sb = (w - 6.0 - ISLAND_H, mid - ISLAND_H / 2.0, ISLAND_H, ISLAND_H);
        self.c.rounded_rect(sb.0, sb.1, sb.2, sb.3, ISLAND_R, if inside(self.pointer, sb) { SUBTLE_HOVER } else { ISLAND });
        let (sx, sy) = (sb.0 + 8.0, mid - 6.0);
        for (i, knob) in [(0.0, 11.0), (6.0, 4.0), (12.0, 9.0)] {
            self.c.rounded_rect(sx, sy + i - 1.0, 16.0, 2.0, 1.0, TEXT_DIM);
            self.c.circle(sx + knob, sy + i, 2.6, TEXT);
        }
        hits.push(Hit { rect: sb, target: Target::Settings });

        let mut parts: Vec<(String, String, f32)> = Vec::new(); // label, value, fill 0..1
        if let Some(c) = sys.cpu {
            parts.push(("CPU".into(), format!("{:.0}%", c * 100.0), c));
        }
        if let Some(m) = sys.mem {
            parts.push(("RAM".into(), format!("{:.0}%", m * 100.0), m));
        }
        if let Some((b, charging)) = sys.battery {
            parts.push((if charging { "CHG".into() } else { "BAT".into() }, format!("{:.0}%", b * 100.0), b));
        }
        if !parts.is_empty() {
            let widths: Vec<f32> = parts
                .iter()
                .map(|(l, v, _)| self.fonts.width(l, 11.0, true) + 6.0 + self.fonts.width(v, 13.0, true) + 22.0)
                .collect();
            let total: f32 = widths.iter().sum::<f32>() + 8.0;
            let x0 = sb.0 - GAP - total;
            self.island(x0, total);
            let mut px = x0 + 12.0;
            for ((label, value, fill), pw) in parts.iter().zip(widths) {
                let lw = self.c.text(self.fonts, label, px, base - 1.0, 11.0, true, TEXT_FAINT);
                self.c.text(self.fonts, value, px + lw + 6.0, base - 0.5, 13.0, true, TEXT);
                // A thin usage bar under the value.
                let bw = pw - 22.0;
                self.c.rounded_rect(px, mid + 9.0, bw, 2.0, 1.0, 0x22ffffff);
                self.c.rounded_rect(px, mid + 9.0, (bw * fill.clamp(0.0, 1.0)).max(2.0), 2.0, 1.0, accent);
                px += pw;
            }
        }
        hits
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for App {
    fn event(app: &mut Self, layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                app.size = (width.max(1), if height == 0 { BAR_HEIGHT } else { height });
                app.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => app.running = false,
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for App {
    fn event(app: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            app.buffer_free[*i] = true;
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
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                app.pointer = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Leave { .. } => app.pointer = None,
            wl_pointer::Event::Button { state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } => {
                app.clicked = app.pointer;
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
