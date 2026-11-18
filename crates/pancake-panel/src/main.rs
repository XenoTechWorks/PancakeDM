//! pancake-panel: the PancakeDM top bar.
//!
//! A floating, rounded bar anchored to the top of the screen (through the
//! wlr-layer-shell protocol): the Pancake button, the open windows (click
//! one to focus it), and the clock. Drawn by hand, no toolkit. Window info
//! comes from the compositor's control socket.

mod draw;

use chrono::Local;
use draw::{Canvas, Fonts};
use memmap2::MmapMut;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

const BAR_HEIGHT: u32 = 40;
const MARGIN: i32 = 8;
const RADIUS: f32 = 14.0;

// Colors (straight alpha).
const BAR_BG: u32 = 0xe61f1a17; // warm near-black glass
const BAR_EDGE: u32 = 0x22ffffff;
const TEXT: u32 = 0xfff6eee3;
const TEXT_DIM: u32 = 0xb3f6eee3;
const GOLD: u32 = 0xfff4a259;
const GOLD_DARK: u32 = 0xffc9792f;
const PILL: u32 = 0x1affffff;
const PILL_HOVER: u32 = 0x2effffff;
const PILL_TEXT_ON_GOLD: u32 = 0xff2a1d12;

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
    window: u32,
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
    use zwlr_layer_surface_v1::Anchor;
    layer.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
    layer.set_size(0, BAR_HEIGHT);
    layer.set_margin(MARGIN, MARGIN, 0, MARGIN);
    layer.set_exclusive_zone(BAR_HEIGHT as i32);
    surface.commit();

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
                let _ = pancake_ipc::send(&format!("focus {}", h.window));
                last_poll = Instant::now() - Duration::from_secs(10);
            }
        }
        if last_poll.elapsed() >= Duration::from_millis(500) {
            windows = pancake_ipc::windows().unwrap_or_default();
            last_poll = Instant::now();
        }

        // Redraw only when something visible changed.
        let now = Local::now();
        let key = format!("{:?}{:?}{}{:?}", app.size, windows, now.format("%H:%M"), app.pointer.map(|(x, y)| (x as i32 / 4, y as i32 / 4)));
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
                let mem = &mut bufs[i].1;
                let px = unsafe { std::slice::from_raw_parts_mut(mem.as_mut_ptr() as *mut u32, (w * h) as usize) };
                let mut canvas = Canvas { px, w: w as i32, h: h as i32 };
                hits = draw_bar(&mut canvas, &mut fonts, &windows, app.pointer);
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
            let mut pfd = PollFd { fd, events: 1, revents: 0 };
            unsafe { poll(&mut pfd, 1, 250) };
            if pfd.revents != 0 {
                guard.read().map_err(|e| e.to_string())?;
            }
        }
        queue.dispatch_pending(&mut app).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

extern "C" {
    fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
}

fn make_buffer(shm: &wl_shm::WlShm, qh: &QueueHandle<App>, w: u32, h: u32, i: usize) -> Result<(wl_buffer::WlBuffer, MmapMut, (u32, u32)), String> {
    let size = (w * h * 4) as u64;
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR isn't set")?;
    let path = std::path::Path::new(&dir).join(format!("pancake-panel-{}-{i}", std::process::id()));
    let file = File::options().read(true).write(true).create(true).truncate(true).open(&path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);
    file.set_len(size).map_err(|e| e.to_string())?;
    let mem = unsafe { MmapMut::map_mut(&file) }.map_err(|e| e.to_string())?;
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888, qh, i);
    pool.destroy();
    Ok((buffer, mem, (w, h)))
}

/// Draw the whole bar; returns the clickable window pills.
fn draw_bar(c: &mut Canvas, fonts: &mut Fonts, windows: &[pancake_ipc::WindowInfo], pointer: Option<(f64, f64)>) -> Vec<Hit> {
    let (w, h) = (c.w as f32, c.h as f32);
    c.clear();
    // Glass bar with a faint light edge.
    c.rounded_rect(0.0, 0.0, w, h, RADIUS, BAR_EDGE);
    c.rounded_rect(1.0, 1.0, w - 2.0, h - 2.0, RADIUS - 1.0, BAR_BG);

    let mid = h / 2.0;
    let baseline = mid + 5.0;

    // The Pancake button: a little golden pancake and the name.
    c.circle(22.0, mid, 9.0, GOLD_DARK);
    c.circle(22.0, mid - 1.0, 7.5, GOLD);
    c.circle(19.5, mid - 3.5, 2.0, 0x55ffffff); // a bit of shine
    let mut x = 38.0;
    x += c.text(fonts, "Pancake", x, baseline, 15.0, true, TEXT) + 18.0;

    // Window pills.
    let mut hits = Vec::new();
    let pill_h = 28.0;
    let max_title = 180.0;
    for win in windows {
        let title = if win.title.is_empty() { "Untitled".to_string() } else { win.title.clone() };
        let title = fonts.fit(&title, 13.0, win.focused, max_title);
        let pw = fonts.width(&title, 13.0, win.focused) + 24.0;
        if x + pw > w / 2.0 - 110.0 {
            break; // leave room for the clock
        }
        let rect = (x, mid - pill_h / 2.0, pw, pill_h);
        let hover = pointer.is_some_and(|(px, py)| {
            let (px, py) = (px as f32, py as f32);
            px >= rect.0 && px < rect.0 + rect.2 && py >= rect.1 && py < rect.1 + rect.3
        });
        let (bg, fg) = match (win.focused, hover) {
            (true, _) => (GOLD, PILL_TEXT_ON_GOLD),
            (false, true) => (PILL_HOVER, TEXT),
            (false, false) => (PILL, TEXT_DIM),
        };
        c.rounded_rect(rect.0, rect.1, rect.2, rect.3, 10.0, bg);
        c.text(fonts, &title, x + 12.0, baseline - 0.5, 13.0, win.focused, fg);
        hits.push(Hit { rect, window: win.id });
        x += pw + 6.0;
    }

    // Clock, centered: dim date, bright time.
    let now = Local::now();
    let date = now.format("%a %-d %b").to_string();
    let time = now.format("%H:%M").to_string();
    let gap = 10.0;
    let total = fonts.width(&date, 14.0, false) + gap + fonts.width(&time, 15.0, true);
    let cx = (w - total) / 2.0;
    let dw = c.text(fonts, &date, cx, baseline, 14.0, false, TEXT_DIM);
    c.text(fonts, &time, cx + dw + gap, baseline, 15.0, true, TEXT);
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
