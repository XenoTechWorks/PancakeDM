//! pancake-launcher: the PancakeDM app launcher.
//!
//! A full-screen overlay (wlr-layer-shell) that dims the desktop and shows
//! a search box and a grid of installed apps. Typing searches, arrow keys
//! move, Enter or a click opens, Esc or a click outside closes. The
//! compositor starts it from the panel's Pancake button or a Super tap.

use pancake_draw::apps::{self, App};
use pancake_draw::color::*;
use pancake_draw::icons::Icons;
use pancake_draw::{Canvas, Fonts};
use std::os::fd::{AsFd, AsRawFd};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use xkbcommon::xkb;

const BACKDROP: u32 = 0x8c120d0a;
const CARD: u32 = 0xfa1f1a17;
const TILE_RING_INNER: u32 = 0xff2b241f;
const TILE_W: f32 = 124.0;
const TILE_H: f32 = 116.0;

// evdev key codes.
const KEY_ESC: u32 = 1;
const KEY_BACKSPACE: u32 = 14;
const KEY_ENTER: u32 = 28;
const KEY_KPENTER: u32 = 96;
const KEY_UP: u32 = 103;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;
const KEY_DOWN: u32 = 108;

struct Launcher {
    apps: Vec<App>,
    query: String,
    results: Vec<usize>,
    selected: usize,
    scroll_row: usize,
    columns: usize,
    rows_visible: usize,
    size: (u32, u32),
    configured: bool,
    running: bool,
    dirty: bool,
    pointer: Option<(f64, f64)>,
    click: Option<(f64, f64)>,
    xkb: Option<xkb::State>,
    launch: Option<usize>,
    buffer_free: [bool; 2],
    /// Tile rectangles from the last draw, with the result index each shows.
    tiles: Vec<((f32, f32, f32, f32), usize)>,
    card: (f32, f32, f32, f32),
    icons: Icons,
    accent: u32,
}

/// Desktop Settings is always listed, even before PancakeDM is installed.
fn builtin_apps() -> Vec<App> {
    let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("pancake-settings"))).filter(|p| p.exists());
    let exec = beside.map(|p| p.display().to_string()).unwrap_or_else(|| "pancake-settings".into());
    vec![App {
        id: "org.pancakedm.Settings.desktop".into(),
        name: "Desktop Settings".into(),
        exec,
        icon: Some("preferences-desktop".into()),
        wm_class: None,
    }]
}

fn main() {
    if let Err(e) = run() {
        eprintln!("pancake-launcher: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut fonts = Fonts::load()?;
    let cfg = pancake_config::load();
    let mut apps = apps::load_all();
    for b in builtin_apps() {
        if !apps.iter().any(|a| a.id == b.id) {
            apps.push(b);
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    let conn = Connection::connect_to_env().map_err(|e| format!("can't connect to Wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<Launcher>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).map_err(|e| format!("wl_compositor: {e}"))?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("wl_shm: {e}"))?;
    let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 =
        globals.bind(&qh, 1..=4, ()).map_err(|_| "the compositor has no layer shell".to_string())?;
    let _seat: Option<wl_seat::WlSeat> = globals.bind(&qh, 1..=5, ()).ok();

    let surface = compositor.create_surface(&qh, ());
    let layer = layer_shell.get_layer_surface(&surface, None, zwlr_layer_shell_v1::Layer::Overlay, "launcher".into(), &qh, ());
    use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer.set_exclusive_zone(-1); // cover everything, panel included
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();

    let results = (0..apps.len()).collect();
    let mut st = Launcher {
        apps,
        query: String::new(),
        results,
        selected: 0,
        scroll_row: 0,
        columns: 1,
        rows_visible: 1,
        size: (0, 0),
        configured: false,
        running: true,
        dirty: true,
        pointer: None,
        click: None,
        xkb: None,
        launch: None,
        buffer_free: [true; 2],
        tiles: Vec::new(),
        card: (0.0, 0.0, 0.0, 0.0),
        icons: Icons::new(&cfg.icon_theme),
        accent: 0xff00_0000 | cfg.accent,
    };
    let mut bufs: Vec<(wl_buffer::WlBuffer, memmap2::MmapMut, (u32, u32))> = Vec::new();
    let fd = conn.backend().poll_fd().as_raw_fd();

    while st.running {
        if let Some((x, y)) = st.click.take() {
            st.handle_click(x as f32, y as f32);
        }
        if let Some(i) = st.launch.take() {
            launch(&st.apps[st.results[i]]);
            break;
        }
        if st.configured && st.dirty {
            let (w, h) = st.size;
            if bufs.first().map_or(true, |b| b.2 != (w, h)) {
                for b in bufs.drain(..) {
                    b.0.destroy();
                }
                for i in 0..2 {
                    let (file, mem) = pancake_draw::shm_file((w * h * 4) as u64, &format!("launcher-{i}"))?;
                    let pool = shm.create_pool(file.as_fd(), (w * h * 4) as i32, &qh, ());
                    let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888, &qh, i);
                    pool.destroy();
                    bufs.push((buffer, mem, (w, h)));
                }
                st.buffer_free = [true; 2];
            }
            if let Some(i) = (0..2).find(|&i| st.buffer_free[i]) {
                let px = pancake_draw::pixels(&mut bufs[i].1);
                let mut canvas = Canvas { px, w: w as i32, h: h as i32 };
                st.draw(&mut canvas, &mut fonts);
                surface.attach(Some(&bufs[i].0), 0, 0);
                surface.damage_buffer(0, 0, w as i32, h as i32);
                surface.commit();
                st.buffer_free[i] = false;
                st.dirty = false;
            }
        }
        conn.flush().map_err(|e| e.to_string())?;
        if let Some(guard) = queue.prepare_read() {
            if pancake_draw::wait_readable(fd, 1000) {
                guard.read().map_err(|e| e.to_string())?;
            }
        }
        queue.dispatch_pending(&mut st).map_err(|e| e.to_string())?;
    }
    conn.flush().ok();
    Ok(())
}

/// Start the app, detached from us: we exit right after.
fn launch(app: &App) {
    use std::process::{Command, Stdio};
    eprintln!("pancake-launcher: starting {} ({})", app.name, app.exec);
    let result = Command::new("sh")
        .arg("-c")
        .arg(&app.exec)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(e) = result {
        eprintln!("pancake-launcher: can't start {}: {e}", app.name);
    }
}

/// A stable badge color per app.
fn badge_color(name: &str) -> u32 {
    let h = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    ACCENTS[(h % ACCENTS.len() as u32) as usize]
}

impl Launcher {
    fn set_query(&mut self, q: String) {
        self.query = q;
        self.results = apps::search(&self.apps, &self.query);
        self.selected = 0;
        self.scroll_row = 0;
        self.dirty = true;
    }

    /// Move the selection by (dx, dy) tiles, keeping it visible.
    fn move_selection(&mut self, dx: i32, dy: i32) {
        if self.results.is_empty() {
            return;
        }
        let cols = self.columns.max(1) as i32;
        let next = self.selected as i32 + dx + dy * cols;
        self.selected = next.clamp(0, self.results.len() as i32 - 1) as usize;
        let row = self.selected / self.columns.max(1);
        if row < self.scroll_row {
            self.scroll_row = row;
        } else if row >= self.scroll_row + self.rows_visible {
            self.scroll_row = row + 1 - self.rows_visible;
        }
        self.dirty = true;
    }

    fn handle_key(&mut self, key: u32) {
        match key {
            KEY_ESC => self.running = false,
            KEY_ENTER | KEY_KPENTER => {
                if !self.results.is_empty() {
                    self.launch = Some(self.selected);
                }
            }
            KEY_BACKSPACE => {
                let mut q = self.query.clone();
                q.pop();
                self.set_query(q);
            }
            KEY_LEFT => self.move_selection(-1, 0),
            KEY_RIGHT => self.move_selection(1, 0),
            KEY_UP => self.move_selection(0, -1),
            KEY_DOWN => self.move_selection(0, 1),
            _ => {
                let text = self.xkb.as_ref().map(|s| s.key_get_utf8(xkb::Keycode::new(key + 8))).unwrap_or_default();
                if !text.is_empty() && !text.chars().any(char::is_control) {
                    let q = format!("{}{text}", self.query);
                    self.set_query(q);
                }
            }
        }
    }

    fn handle_click(&mut self, x: f32, y: f32) {
        let (cx, cy, cw, ch) = self.card;
        if x < cx || y < cy || x >= cx + cw || y >= cy + ch {
            self.running = false; // click outside the card closes
            return;
        }
        if let Some(&(_, i)) = self.tiles.iter().find(|((tx, ty, tw, th), _)| x >= *tx && x < tx + tw && y >= *ty && y < ty + th) {
            self.launch = Some(i);
        }
    }

    fn draw(&mut self, c: &mut Canvas, fonts: &mut Fonts) {
        let (w, h) = (c.w as f32, c.h as f32);
        c.px.fill(0);
        c.rounded_rect(0.0, 0.0, w, h, 0.0, BACKDROP);

        // The card.
        let cw = (w - 80.0).min(880.0);
        let ch = (h - 120.0).min(640.0);
        let (cx, cy) = ((w - cw) / 2.0, (h - ch) / 2.0);
        self.card = (cx, cy, cw, ch);
        c.rounded_rect(cx - 1.0, cy - 1.0, cw + 2.0, ch + 2.0, 25.0, GLASS_EDGE);
        c.rounded_rect(cx, cy, cw, ch, 24.0, CARD);

        // Search box.
        let pad = 24.0;
        let (sx, sy, sw, sh) = (cx + pad, cy + pad, cw - pad * 2.0, 48.0);
        c.rounded_rect(sx, sy, sw, sh, 14.0, SUBTLE);
        // A little magnifier: ring and handle.
        c.circle(sx + 24.0, sy + 22.0, 8.0, TEXT_DIM);
        c.circle(sx + 24.0, sy + 22.0, 5.5, TILE_RING_INNER);
        c.rounded_rect(sx + 29.0, sy + 27.0, 7.0, 3.0, 1.5, TEXT_DIM);
        let baseline = sy + sh / 2.0 + 6.0;
        if self.query.is_empty() {
            c.text(fonts, "Search apps…", sx + 46.0, baseline, 17.0, false, TEXT_FAINT);
        } else {
            let tw = c.text(fonts, &self.query, sx + 46.0, baseline, 17.0, false, TEXT);
            c.rounded_rect(sx + 48.0 + tw, sy + 13.0, 2.0, 22.0, 1.0, self.accent); // caret
        }

        // Grid of apps.
        let gx = cx + pad;
        let gy = sy + sh + 20.0;
        let gw = cw - pad * 2.0;
        let gh = cy + ch - gy - 44.0;
        self.columns = ((gw / TILE_W) as usize).max(1);
        self.rows_visible = ((gh / TILE_H) as usize).max(1);
        let x0 = gx + (gw - self.columns as f32 * TILE_W) / 2.0;
        self.tiles.clear();
        if self.results.is_empty() {
            let msg = "No apps match";
            let mw = fonts.width(msg, 15.0, false);
            c.text(fonts, msg, cx + (cw - mw) / 2.0, gy + 60.0, 15.0, false, TEXT_DIM);
        }
        let first = self.scroll_row * self.columns;
        let last = (first + self.columns * self.rows_visible).min(self.results.len());
        for i in first..last {
            let slot = i - first;
            let (col, row) = (slot % self.columns, slot / self.columns);
            let (tx, ty) = (x0 + col as f32 * TILE_W, gy + row as f32 * TILE_H);
            let rect = (tx + 4.0, ty + 4.0, TILE_W - 8.0, TILE_H - 8.0);
            let app = self.apps[self.results[i]].clone();
            let hover = self.pointer.is_some_and(|(px, py)| {
                let (px, py) = (px as f32, py as f32);
                px >= rect.0 && px < rect.0 + rect.2 && py >= rect.1 && py < rect.1 + rect.3
            });
            if i == self.selected {
                c.rounded_rect(rect.0, rect.1, rect.2, rect.3, 16.0, self.accent);
                c.rounded_rect(rect.0 + 2.0, rect.1 + 2.0, rect.2 - 4.0, rect.3 - 4.0, 14.0, TILE_RING_INNER);
            } else if hover {
                c.rounded_rect(rect.0, rect.1, rect.2, rect.3, 16.0, SUBTLE);
            }
            // The app's icon, or a letter badge if it has none we can find.
            let (bx, by) = (tx + TILE_W / 2.0, ty + 42.0);
            let icon = app.icon.as_ref().and_then(|n| self.icons.get(n, 56));
            if let Some(img) = icon {
                c.image(img, (bx - 28.0) as i32, (by - 28.0) as i32);
            } else {
                c.circle(bx, by, 27.0, badge_color(&app.name));
                let letter: String = app.name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
                let lw = fonts.width(&letter, 24.0, true);
                c.text(fonts, &letter, bx - lw / 2.0, by + 8.5, 24.0, true, 0xffffffff);
            }
            // Name, centered and cut to fit.
            let name = fonts.fit(&app.name, 13.0, false, TILE_W - 16.0);
            let nw = fonts.width(&name, 13.0, false);
            c.text(fonts, &name, bx - nw / 2.0, ty + 94.0, 13.0, false, TEXT);
            self.tiles.push((rect, i));
        }

        // Hint line.
        let hint = format!("{} apps  ·  Arrows to move  ·  Enter to open  ·  Esc to close", self.results.len());
        let hw = fonts.width(&hint, 12.0, false);
        c.text(fonts, &hint, cx + (cw - hw) / 2.0, cy + ch - 18.0, 12.0, false, TEXT_FAINT);
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Launcher {
    fn event(st: &mut Self, layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                st.size = (width.max(1), height.max(1));
                st.configured = true;
                st.dirty = true;
            }
            zwlr_layer_surface_v1::Event::Closed => st.running = false,
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Launcher {
    fn event(st: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                // Safety: the compositor sent a keymap of `size` bytes.
                let keymap = unsafe { xkb::Keymap::new_from_fd(&ctx, fd, size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS) };
                if let Ok(Some(km)) = keymap {
                    st.xkb = Some(xkb::State::new(&km));
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(s) = &mut st.xkb {
                    s.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => st.handle_key(key),
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Launcher {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                st.pointer = Some((surface_x, surface_y));
                st.dirty = true;
            }
            wl_pointer::Event::Leave { .. } => st.pointer = None,
            wl_pointer::Event::Button { state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } => st.click = st.pointer,
            wl_pointer::Event::Axis { value, .. } => {
                let rows = st.results.len().div_ceil(st.columns.max(1));
                let max = rows.saturating_sub(st.rows_visible);
                st.scroll_row = if value > 0.0 { (st.scroll_row + 1).min(max) } else { st.scroll_row.saturating_sub(1) };
                st.dirty = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Launcher {
    fn event(_: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for Launcher {
    fn event(st: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            st.buffer_free[*i] = true;
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Launcher {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(Launcher: ignore wl_compositor::WlCompositor);
delegate_noop!(Launcher: ignore wl_surface::WlSurface);
delegate_noop!(Launcher: ignore wl_shm::WlShm);
delegate_noop!(Launcher: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Launcher: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
