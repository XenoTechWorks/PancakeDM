//! pancake-panel: the PancakeDM top bar.
//!
//! A floating, rounded bar anchored to the top of the screen (through the
//! wlr-layer-shell protocol): the Pancake button, the open windows (click
//! one to focus it), and the clock. Drawn by hand, no toolkit. Window info
//! comes from the compositor's control socket; position, clock style and
//! accent come from pancake.conf and update live.

use chrono::Local;
use memmap2::MmapMut;
use pancake_draw::color::*;
use pancake_config::{Config, PanelPosition};
use pancake_draw::icons::Icons;
use pancake_draw::{Canvas, Fonts};
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

const BAR_HEIGHT: u32 = 40;
const MARGIN: i32 = 8;
const RADIUS: f32 = 14.0;


struct App {
    configured: bool,
    size: (u32, u32),
    running: bool,
    pointer: Option<(f64, f64)>,
    clicked: Option<(f64, f64)>,
    buffer_free: [bool; 2],
}

/// Clickable things drawn this frame.
struct Hit {
    rect: (f32, f32, f32, f32),
    target: Target,
}

enum Target {
    Launcher,
    Window(u32),
    Settings,
}

/// Straight-alpha 0xAARRGGBB from a config 0xRRGGBB.
fn opaque(rgb: u32) -> u32 {
    0xff00_0000 | rgb
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
    let mut watch = pancake_config::Watcher::new();
    place(&layer, cfg.panel_position);
    layer.set_size(0, BAR_HEIGHT);
    layer.set_exclusive_zone(BAR_HEIGHT as i32);
    surface.commit();
    let apps = pancake_draw::apps::load_all();
    let mut icons = Icons::new(&cfg.icon_theme);
    let mut settings: Option<std::process::Child> = None;

    let mut app = App { configured: false, size: (0, BAR_HEIGHT), running: true, pointer: None, clicked: None, buffer_free: [true; 2] };
    let mut bufs: Vec<(wl_buffer::WlBuffer, MmapMut, (u32, u32))> = Vec::new();
    let mut windows: Vec<pancake_ipc::WindowInfo> = Vec::new();
    let mut hits: Vec<Hit> = Vec::new();
    let mut last_poll = Instant::now() - Duration::from_secs(10);
    let mut last_drawn = String::new();
    let fd = conn.backend().poll_fd().as_raw_fd();

    while app.running {
        // Clicks on window pills: focus that window.
        if let Some((x, y)) = app.clicked.take() {
            let (x, y) = (x as f32, y as f32);
            if let Some(h) = hits.iter().find(|h| x >= h.rect.0 && x < h.rect.0 + h.rect.2 && y >= h.rect.1 && y < h.rect.1 + h.rect.3) {
                match h.target {
                    Target::Launcher => drop(pancake_ipc::send("launcher")),
                    Target::Window(id) => drop(pancake_ipc::send(&format!("focus {id}"))),
                    Target::Settings => open_settings(&mut settings),
                }
                last_poll = Instant::now() - Duration::from_secs(10);
            }
        }
        if last_poll.elapsed() >= Duration::from_millis(500) {
            windows = pancake_ipc::windows().unwrap_or_default();
            last_poll = Instant::now();
            if watch.changed() {
                let new = pancake_config::load();
                if new.panel_position != cfg.panel_position {
                    place(&layer, new.panel_position);
                    surface.commit();
                }
                icons.set_theme(&new.icon_theme);
                cfg = new;
                last_drawn.clear();
            }
        }

        // Redraw only when something visible changed.
        let now = Local::now();
        let key = format!("{:?}{:?}{}{:?}{:?}", app.size, windows, now.format("%H:%M"), app.pointer.map(|(x, y)| (x as i32 / 4, y as i32 / 4)), cfg);
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
                hits = draw_bar(&mut canvas, &mut fonts, &mut icons, &apps, &cfg, &windows, app.pointer);
                surface.attach(Some(&bufs[i].0), 0, 0);
                surface.damage_buffer(0, 0, w as i32, h as i32);
                surface.commit();
                app.buffer_free[i] = false;
                last_drawn = key;
            }
        }

        conn.flush().map_err(|e| e.to_string())?;
        // Wake for input, the next poll of the window list, or the next minute.
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

fn inside(p: Option<(f64, f64)>, r: (f32, f32, f32, f32)) -> bool {
    p.is_some_and(|(x, y)| {
        let (x, y) = (x as f32, y as f32);
        x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
    })
}

/// Draw the whole bar; returns what's clickable.
fn draw_bar(
    c: &mut Canvas,
    fonts: &mut Fonts,
    icons: &mut Icons,
    apps: &[pancake_draw::apps::App],
    cfg: &Config,
    windows: &[pancake_ipc::WindowInfo],
    pointer: Option<(f64, f64)>,
) -> Vec<Hit> {
    let accent = opaque(cfg.accent);
    let (w, h) = (c.w as f32, c.h as f32);
    c.clear();
    // Glass bar with a faint light edge.
    c.rounded_rect(0.0, 0.0, w, h, RADIUS, GLASS_EDGE);
    c.rounded_rect(1.0, 1.0, w - 2.0, h - 2.0, RADIUS - 1.0, GLASS);

    let mid = h / 2.0;
    let baseline = mid + 5.0;

    // The Pancake button (opens the launcher): a golden pancake and the name.
    let mut hits = Vec::new();
    let button_w = 38.0 + fonts.width("Pancake", 15.0, true) + 12.0;
    let button = (4.0, mid - 15.0, button_w, 30.0);
    if inside(pointer, button) {
        c.rounded_rect(button.0, button.1, button.2, button.3, 11.0, SUBTLE);
    }
    hits.push(Hit { rect: button, target: Target::Launcher });
    c.circle(22.0, mid, 9.0, GOLD_DARK);
    c.circle(22.0, mid - 1.0, 7.5, GOLD);
    c.circle(19.5, mid - 3.5, 2.0, 0x55ffffff); // a bit of shine
    let mut x = 38.0;
    x += c.text(fonts, "Pancake", x, baseline, 15.0, true, TEXT) + 18.0;

    // Window pills.
    let pill_h = 28.0;
    let max_title = 180.0;
    for win in windows {
        let title = if win.title.is_empty() { "Untitled".to_string() } else { win.title.clone() };
        let title = fonts.fit(&title, 13.0, win.focused, max_title);
        // The app's icon, through its desktop entry (or its id as icon name).
        let icon_name = pancake_draw::apps::for_app_id(apps, &win.app_id)
            .and_then(|a| a.icon.clone())
            .or_else(|| (!win.app_id.is_empty()).then(|| win.app_id.clone()));
        let has_icon = icon_name.as_ref().is_some_and(|n| icons.get(n, 18).is_some());
        let icon_space = if has_icon { 24.0 } else { 0.0 };
        let pw = fonts.width(&title, 13.0, win.focused) + 24.0 + icon_space;
        if x + pw > w / 2.0 - 110.0 {
            break; // leave room for the clock
        }
        let rect = (x, mid - pill_h / 2.0, pw, pill_h);
        let hover = inside(pointer, rect);
        let (bg, fg) = match (win.focused, hover) {
            (true, _) => (accent, text_on(cfg.accent)),
            (false, true) => (SUBTLE_HOVER, TEXT),
            (false, false) => (SUBTLE, TEXT_DIM),
        };
        c.rounded_rect(rect.0, rect.1, rect.2, rect.3, 10.0, bg);
        if let (true, Some(name)) = (has_icon, &icon_name) {
            if let Some(img) = icons.get(name, 18) {
                c.image(img, (x + 10.0) as i32, (mid - 9.0) as i32);
            }
        }
        c.text(fonts, &title, x + 12.0 + icon_space, baseline - 0.5, 13.0, win.focused, fg);
        hits.push(Hit { rect, target: Target::Window(win.id) });
        x += pw + 6.0;
    }

    // Clock, centered: dim date, bright time.
    let now = Local::now();
    let date = if cfg.show_date { now.format("%a %-d %b").to_string() } else { String::new() };
    let time = if cfg.clock_24h { now.format("%H:%M") } else { now.format("%-I:%M %p") }.to_string();
    let gap = if date.is_empty() { 0.0 } else { 10.0 };
    let total = fonts.width(&date, 14.0, false) + gap + fonts.width(&time, 15.0, true);
    let cx = (w - total) / 2.0;
    let dw = c.text(fonts, &date, cx, baseline, 14.0, false, TEXT_DIM);
    c.text(fonts, &time, cx + dw + gap, baseline, 15.0, true, TEXT);

    // Settings button on the right: three slider lines.
    let sb = (w - 40.0, mid - 15.0, 32.0, 30.0);
    if inside(pointer, sb) {
        c.rounded_rect(sb.0, sb.1, sb.2, sb.3, 11.0, SUBTLE);
    }
    let (sx, sy) = (sb.0 + 8.0, mid - 6.0);
    for (i, knob) in [(0.0, 11.0), (6.0, 4.0), (12.0, 9.0)] {
        c.rounded_rect(sx, sy + i - 1.0, 16.0, 2.0, 1.0, TEXT_DIM);
        c.circle(sx + knob, sy + i, 2.6, TEXT);
    }
    hits.push(Hit { rect: sb, target: Target::Settings });
    hits
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
