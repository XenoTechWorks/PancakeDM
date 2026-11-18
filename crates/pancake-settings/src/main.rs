//! pancake-settings: Desktop Settings.
//!
//! Wallpaper (with fill/fit/center/stretch), panel position, clock style,
//! date, and accent color. Every change is written to pancake.conf right
//! away; the compositor and panel notice and apply it live. Drawn by hand
//! with pancake-draw, like the rest of the shell.

use pancake_config::{Config, Layout, PanelPosition, WallpaperMode};
use pancake_draw::color::*;
use pancake_draw::images::{self, Image};
use pancake_draw::{Canvas, Fonts};
use std::os::fd::{AsFd, AsRawFd};
use std::path::PathBuf;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const W: u32 = 780;
const H: u32 = 700;
const RADIUS: f32 = 18.0;
const WINDOW_BG: u32 = 0xff221c18;
const HEADER_BG: u32 = 0xff2a231e;
const THUMB: (u32, u32) = (156, 98);
/// Wallpaper tiles per page (one row; the last tile overall is "None").
const WALLS_PER_PAGE: usize = 4;

#[derive(Clone, PartialEq)]
enum Action {
    Drag,
    Close,
    Wallpaper(Option<PathBuf>),
    Mode(WallpaperMode),
    Position(PanelPosition),
    Clock24(bool),
    ToggleDate,
    Accent(u32),
    AccentAuto,
    Layout(Layout),
    /// Page through the wallpapers (-1 / +1).
    Page(i32),
}

struct App {
    configured: bool,
    running: bool,
    pointer: Option<(f64, f64)>,
    click: Option<(f64, f64, u32)>,
    buffer_free: [bool; 2],
    dirty: bool,
    seat: Option<wl_seat::WlSeat>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("pancake-settings: {e}");
        std::process::exit(1);
    }
}

/// Round the corners of opaque pixels (premultiplied, so alpha scales all).
fn round_corners(img: &mut Image, r: f32) {
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

fn thumbnail(path: &PathBuf) -> Option<Image> {
    let img = images::load(path, None)?;
    let px = images::wallpaper(&img, THUMB.0, THUMB.1, images::Fit::Fill, 0);
    let mut t = Image { w: THUMB.0, h: THUMB.1, px };
    round_corners(&mut t, 12.0);
    Some(t)
}

/// How to store a wallpaper: just the name if it's in a wallpaper folder.
fn wallpaper_value(p: &PathBuf) -> String {
    let in_dir = pancake_config::wallpaper_dirs().iter().any(|d| p.parent() == Some(d.as_path()));
    match (in_dir, p.file_name()) {
        (true, Some(n)) => n.to_string_lossy().into_owned(),
        _ => p.display().to_string(),
    }
}

fn run() -> Result<(), String> {
    let mut fonts = Fonts::load()?;
    let mut cfg = pancake_config::load();
    let mut watch = pancake_config::Watcher::new();
    let walls: Vec<(PathBuf, Option<Image>)> = pancake_config::list_wallpapers().into_iter().map(|p| {
        let t = thumbnail(&p);
        (p, t)
    }).collect();

    let conn = Connection::connect_to_env().map_err(|e| format!("can't connect to Wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).map_err(|e| format!("wl_compositor: {e}"))?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("wl_shm: {e}"))?;
    let wm_base: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=3, ()).map_err(|e| format!("xdg_wm_base: {e}"))?;
    let seat: Option<wl_seat::WlSeat> = globals.bind(&qh, 1..=5, ()).ok();

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("Desktop Settings".into());
    toplevel.set_app_id("org.pancakedm.Settings".into());
    toplevel.set_min_size(W as i32, H as i32);
    toplevel.set_max_size(W as i32, H as i32);
    surface.commit();

    let mut bufs = Vec::new();
    for i in 0..2 {
        let (file, mem) = pancake_draw::shm_file((W * H * 4) as u64, &format!("settings-{i}"))?;
        let pool = shm.create_pool(file.as_fd(), (W * H * 4) as i32, &qh, ());
        let buffer = pool.create_buffer(0, W as i32, H as i32, (W * 4) as i32, wl_shm::Format::Argb8888, &qh, i);
        pool.destroy();
        bufs.push((buffer, mem));
    }

    let mut app = App { configured: false, running: true, pointer: None, click: None, buffer_free: [true; 2], dirty: true, seat };
    let mut hits: Vec<((f32, f32, f32, f32), Action)> = Vec::new();
    let fd = conn.backend().poll_fd().as_raw_fd();
    // Start on the page that has the current wallpaper.
    let mut page = {
        let current = cfg.wallpaper_path();
        let idx = walls.iter().position(|(p, _)| Some(p) == current.as_ref()).unwrap_or(walls.len());
        idx / WALLS_PER_PAGE
    };

    while app.running {
        if let Some((x, y, serial)) = app.click.take() {
            let (x, y) = (x as f32, y as f32);
            let hit = hits.iter().find(|(r, _)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3).map(|(_, a)| a.clone());
            match hit {
                Some(Action::Drag) => {
                    if let Some(s) = &app.seat {
                        toplevel._move(s, serial);
                    }
                }
                Some(Action::Close) => app.running = false,
                Some(Action::Page(d)) => {
                    let pages = (walls.len() + 1).div_ceil(WALLS_PER_PAGE);
                    page = (page as i32 + d).clamp(0, pages as i32 - 1) as usize;
                    app.dirty = true;
                }
                Some(a) => {
                    apply(&mut cfg, a);
                    if let Err(e) = pancake_config::save(&cfg) {
                        eprintln!("pancake-settings: can't save settings: {e}");
                    }
                    watch.changed(); // our own write; don't reload it
                    app.dirty = true;
                }
                None => {}
            }
        }
        if watch.changed() {
            cfg = pancake_config::load(); // edited by hand meanwhile
            app.dirty = true;
        }
        if app.configured && app.dirty {
            if let Some(i) = (0..2).find(|&i| app.buffer_free[i]) {
                let px = pancake_draw::pixels(&mut bufs[i].1);
                let mut c = Canvas { px, w: W as i32, h: H as i32 };
                hits = draw(&mut c, &mut fonts, &cfg, &walls, page, app.pointer);
                surface.attach(Some(&bufs[i].0), 0, 0);
                surface.damage_buffer(0, 0, W as i32, H as i32);
                surface.commit();
                app.buffer_free[i] = false;
                app.dirty = false;
            }
        }
        conn.flush().map_err(|e| e.to_string())?;
        if let Some(guard) = queue.prepare_read() {
            if pancake_draw::wait_readable(fd, 500) {
                guard.read().map_err(|e| e.to_string())?;
            }
        }
        queue.dispatch_pending(&mut app).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn apply(cfg: &mut Config, a: Action) {
    match a {
        Action::Wallpaper(Some(p)) => cfg.wallpaper = wallpaper_value(&p),
        Action::Wallpaper(None) => cfg.wallpaper.clear(),
        Action::Mode(m) => cfg.wallpaper_mode = m,
        Action::Position(p) => cfg.panel_position = p,
        Action::Clock24(b) => cfg.clock_24h = b,
        Action::ToggleDate => cfg.show_date = !cfg.show_date,
        Action::Accent(c) => {
            cfg.accent = c;
            cfg.accent_auto = false;
        }
        Action::AccentAuto => cfg.accent_auto = true,
        Action::Layout(l) => cfg.layout = l,
        Action::Drag | Action::Close | Action::Page(_) => {}
    }
}

fn inside(p: Option<(f64, f64)>, r: (f32, f32, f32, f32)) -> bool {
    p.is_some_and(|(x, y)| {
        let (x, y) = (x as f32, y as f32);
        x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
    })
}

/// A row of options, one selected. Returns the hit areas.
fn segmented(c: &mut Canvas, fonts: &mut Fonts, x: f32, y: f32, items: &[(&str, Action, bool)], accent: u32, pointer: Option<(f64, f64)>) -> Vec<((f32, f32, f32, f32), Action)> {
    let widths: Vec<f32> = items.iter().map(|(l, _, _)| fonts.width(l, 13.0, true) + 28.0).collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0;
    c.rounded_rect(x, y, total, 34.0, 11.0, SUBTLE);
    let mut hits = Vec::new();
    let mut cx = x + 3.0;
    for ((label, action, on), w) in items.iter().zip(widths) {
        let r = (cx, y + 3.0, w, 28.0);
        if *on {
            c.rounded_rect(r.0, r.1, r.2, r.3, 9.0, accent);
        } else if inside(pointer, r) {
            c.rounded_rect(r.0, r.1, r.2, r.3, 9.0, SUBTLE_HOVER);
        }
        let tw = fonts.width(label, 13.0, true);
        let fg = if *on { text_on(accent & 0xff_ffff) } else { TEXT };
        c.text(fonts, label, cx + (w - tw) / 2.0, y + 22.0, 13.0, true, fg);
        hits.push((r, action.clone()));
        cx += w;
    }
    hits
}

/// A small round button with a chevron, for paging.
fn arrow_button(c: &mut Canvas, r: (f32, f32, f32, f32), left: bool, enabled: bool, pointer: Option<(f64, f64)>) {
    let bg = if enabled && inside(pointer, r) { SUBTLE_HOVER } else { SUBTLE };
    c.circle(r.0 + r.2 / 2.0, r.1 + r.3 / 2.0, r.2 / 2.0, bg);
    let fg = if enabled { TEXT } else { TEXT_FAINT };
    let (cx, cy) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
    let dir = if left { 1.0 } else { -1.0 };
    for i in -3..=3 {
        let t = i as f32;
        c.circle(cx + dir * (t.abs() - 1.5), cy + t * 1.2, 1.1, fg);
    }
}

fn draw(c: &mut Canvas, fonts: &mut Fonts, cfg: &Config, walls: &[(PathBuf, Option<Image>)], page: usize, pointer: Option<(f64, f64)>) -> Vec<((f32, f32, f32, f32), Action)> {
    let auto_accent = pancake_draw::accent::effective(&Config { accent_auto: true, ..cfg.clone() });
    let accent = 0xff00_0000 | if cfg.accent_auto { auto_accent } else { cfg.accent };
    let (w, h) = (c.w as f32, c.h as f32);
    let mut hits = Vec::new();
    c.px.fill(0);
    c.rounded_rect(0.0, 0.0, w, h, RADIUS, 0x33ffffff);
    c.rounded_rect(1.0, 1.0, w - 2.0, h - 2.0, RADIUS - 1.0, WINDOW_BG);

    // Header: title, close button; drag it to move the window.
    c.rounded_rect(1.0, 1.0, w - 2.0, 60.0, RADIUS - 1.0, HEADER_BG);
    c.rounded_rect(1.0, 40.0, w - 2.0, 21.0, 0.0, HEADER_BG);
    c.text(fonts, "Desktop Settings", 28.0, 38.0, 18.0, true, TEXT);
    let close = (w - 50.0, 15.0, 32.0, 32.0);
    c.circle(close.0 + 16.0, close.1 + 16.0, 14.0, if inside(pointer, close) { 0xffbc4b51 } else { SUBTLE });
    // The "×": two short bars drawn as dots along each diagonal.
    for i in -4..=4 {
        let d = i as f32 * 1.1;
        c.circle(close.0 + 16.0 + d, close.1 + 16.0 + d, 1.1, TEXT);
        c.circle(close.0 + 16.0 + d, close.1 + 16.0 - d, 1.1, TEXT);
    }
    hits.push((close, Action::Close));
    hits.push(((0.0, 0.0, w - 60.0, 61.0), Action::Drag));

    let pad = 28.0;
    let section = |c: &mut Canvas, fonts: &mut Fonts, label: &str, y: f32| {
        c.text(fonts, label, pad, y, 15.0, true, TEXT);
    };

    // Wallpaper.
    let mut y = 96.0;
    section(c, fonts, "Wallpaper", y);
    y += 14.0;
    let current = cfg.wallpaper_path();
    let mut x = pad;
    let mut tiles: Vec<(Option<&PathBuf>, Option<&Image>)> = walls.iter().map(|(p, t)| (Some(p), t.as_ref())).collect();
    tiles.push((None, None)); // "None": just the background color
    // Paging, top right of the section: ‹ 2 / 4 ›
    let pages = tiles.len().div_ceil(WALLS_PER_PAGE);
    if pages > 1 {
        let label = format!("{} / {}", page + 1, pages);
        let lw = fonts.width(&label, 13.0, false);
        let next = (w - pad - 28.0, y - 20.0, 28.0, 28.0);
        let prev = (next.0 - lw - 44.0, next.1, 28.0, 28.0);
        arrow_button(c, prev, true, page > 0, pointer);
        arrow_button(c, next, false, page + 1 < pages, pointer);
        c.text(fonts, &label, prev.0 + 36.0, y - 1.0, 13.0, false, TEXT_DIM);
        hits.push((prev, Action::Page(-1)));
        hits.push((next, Action::Page(1)));
    }
    let first = page * WALLS_PER_PAGE;
    for (path, thumb) in tiles.into_iter().skip(first).take(WALLS_PER_PAGE) {
        let r = (x, y, THUMB.0 as f32, THUMB.1 as f32);
        let selected = match path {
            Some(p) => current.as_ref() == Some(p),
            None => current.is_none(),
        };
        if selected {
            c.rounded_rect(r.0 - 4.0, r.1 - 4.0, r.2 + 8.0, r.3 + 8.0, 15.0, accent);
            c.rounded_rect(r.0 - 1.0, r.1 - 1.0, r.2 + 2.0, r.3 + 2.0, 13.0, WINDOW_BG);
        } else if inside(pointer, r) {
            c.rounded_rect(r.0 - 4.0, r.1 - 4.0, r.2 + 8.0, r.3 + 8.0, 15.0, SUBTLE_HOVER);
        }
        match thumb {
            Some(t) => c.image(t, r.0 as i32, r.1 as i32),
            None => {
                c.rounded_rect(r.0, r.1, r.2, r.3, 12.0, 0xff00_0000 | cfg.background);
                let label = if path.is_some() { "Can't load" } else { "No wallpaper" };
                let tw = fonts.width(label, 13.0, true);
                c.text(fonts, label, r.0 + (r.2 - tw) / 2.0, r.1 + r.3 / 2.0 + 5.0, 13.0, true, ON_GOLD);
            }
        }
        hits.push((r, Action::Wallpaper(path.cloned())));
        x += THUMB.0 as f32 + 16.0;
    }
    y += THUMB.1 as f32 + 22.0;
    let modes: Vec<(&str, Action, bool)> = [("Fill", WallpaperMode::Fill), ("Fit", WallpaperMode::Fit), ("Center", WallpaperMode::Center), ("Stretch", WallpaperMode::Stretch)]
        .into_iter()
        .map(|(l, m)| (l, Action::Mode(m), cfg.wallpaper_mode == m))
        .collect();
    hits.extend(segmented(c, fonts, pad, y, &modes, accent, pointer));

    // Panel.
    y += 74.0;
    section(c, fonts, "Panel", y);
    let label_x = pad;
    let ctrl_x = 200.0;
    y += 18.0;
    c.text(fonts, "Position", label_x, y + 22.0, 14.0, false, TEXT_DIM);
    let pos = cfg.panel_position;
    hits.extend(segmented(c, fonts, ctrl_x, y, &[("Top", Action::Position(PanelPosition::Top), pos == PanelPosition::Top), ("Bottom", Action::Position(PanelPosition::Bottom), pos == PanelPosition::Bottom)], accent, pointer));
    y += 46.0;
    c.text(fonts, "Clock", label_x, y + 22.0, 14.0, false, TEXT_DIM);
    hits.extend(segmented(c, fonts, ctrl_x, y, &[("24-hour", Action::Clock24(true), cfg.clock_24h), ("12-hour", Action::Clock24(false), !cfg.clock_24h)], accent, pointer));
    y += 46.0;
    c.text(fonts, "Show date", label_x, y + 22.0, 14.0, false, TEXT_DIM);
    // A switch.
    let sw = (ctrl_x, y + 4.0, 48.0, 26.0);
    c.rounded_rect(sw.0, sw.1, sw.2, sw.3, 13.0, if cfg.show_date { accent } else { SUBTLE_HOVER });
    let knob_x = if cfg.show_date { sw.0 + sw.2 - 13.0 } else { sw.0 + 13.0 };
    c.circle(knob_x, sw.1 + 13.0, 10.0, 0xfff6eee3);
    hits.push((sw, Action::ToggleDate));

    // Windows.
    y += 70.0;
    section(c, fonts, "Windows", y);
    y += 18.0;
    c.text(fonts, "New windows", label_x, y + 22.0, 14.0, false, TEXT_DIM);
    hits.extend(segmented(c, fonts, ctrl_x, y, &[("Tile", Action::Layout(Layout::Tiling), cfg.layout == Layout::Tiling), ("Float", Action::Layout(Layout::Floating), cfg.layout == Layout::Floating)], accent, pointer));

    // Accent color: "Auto" (from the wallpaper) first, then fixed colors.
    y += 70.0;
    section(c, fonts, "Accent color", y);
    y += 16.0;
    {
        let (cx, cy) = (pad + 18.0, y + 20.0);
        if cfg.accent_auto {
            c.circle(cx, cy, 19.0, 0xfff6eee3);
            c.circle(cx, cy, 16.5, WINDOW_BG);
        } else if inside(pointer, (cx - 16.0, cy - 16.0, 32.0, 32.0)) {
            c.circle(cx, cy, 19.0, SUBTLE_HOVER);
        }
        c.circle(cx, cy, 14.0, 0xff00_0000 | auto_accent);
        let aw = fonts.width("A", 13.0, true);
        c.text(fonts, "A", cx - aw / 2.0, cy + 5.0, 13.0, true, text_on(auto_accent));
        hits.push(((cx - 16.0, cy - 16.0, 32.0, 32.0), Action::AccentAuto));
        c.text(fonts, "Auto uses your wallpaper's color", pad + 360.0, cy + 5.0, 12.0, false, TEXT_FAINT);
    }
    for (i, &col) in pancake_config::ACCENTS.iter().enumerate() {
        let (cx, cy) = (pad + 18.0 + (i + 1) as f32 * 48.0, y + 20.0);
        if !cfg.accent_auto && cfg.accent == col {
            c.circle(cx, cy, 19.0, 0xfff6eee3);
            c.circle(cx, cy, 16.5, WINDOW_BG);
        } else if inside(pointer, (cx - 16.0, cy - 16.0, 32.0, 32.0)) {
            c.circle(cx, cy, 19.0, SUBTLE_HOVER);
        }
        c.circle(cx, cy, 14.0, 0xff00_0000 | col);
        hits.push(((cx - 16.0, cy - 16.0, 32.0, 32.0), Action::Accent(col)));
    }

    // Footer.
    let note = format!("Saved to {}", pancake_config::config_path().display());
    c.text(fonts, &note, pad, h - 20.0, 12.0, false, TEXT_FAINT);
    hits
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(_: &mut Self, base: &xdg_wm_base::XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(app: &mut Self, xdg: &xdg_surface::XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            app.configured = true;
            app.dirty = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(app: &mut Self, _: &xdg_toplevel::XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_toplevel::Event::Close = event {
            app.running = false;
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
                app.dirty = true;
            }
            wl_pointer::Event::Leave { .. } => {
                app.pointer = None;
                app.dirty = true;
            }
            wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } => {
                app.click = app.pointer.map(|(x, y)| (x, y, serial));
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
