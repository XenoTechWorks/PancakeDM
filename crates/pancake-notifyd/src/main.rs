//! pancake-notifyd: desktop notifications and calendar reminders.
//!
//! Implements org.freedesktop.Notifications (what `notify-send` and apps
//! use) when no other daemon owns that name, and shows toasts in the
//! top-right corner. Calendar reminders from `pancake-event` show as
//! toasts too, even when another daemon handles app notifications. Click a
//! toast to dismiss it.

mod reminders;

use chrono::Local;
use dbus::blocking::Connection;
use dbus_crossroads::Crossroads;
use pancake_config::PanelPosition;
use pancake_draw::color::*;
use pancake_draw::{Canvas, Fonts};
use std::collections::HashSet;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection as WlConn, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

const W: u32 = 372;
const ROW: u32 = 88;
const GAP: u32 = 10;
const MAX_SHOWN: usize = 4;
const DEFAULT_SECS: u64 = 7;

#[derive(Clone)]
struct Toast {
    id: u32,
    app: String,
    summary: String,
    body: String,
    shown: Instant,
    /// None: stays until clicked.
    lifetime: Option<Duration>,
}

#[derive(Default)]
struct Store {
    toasts: Vec<Toast>,
    next_id: u32,
}

impl Store {
    fn add(&mut self, replaces: u32, app: &str, summary: &str, body: &str, timeout_ms: i32) -> u32 {
        let lifetime = match timeout_ms {
            0 => None,
            t if t > 0 => Some(Duration::from_millis(t as u64)),
            _ => Some(Duration::from_secs(DEFAULT_SECS)),
        };
        let id = if replaces != 0 && self.toasts.iter().any(|t| t.id == replaces) {
            replaces
        } else {
            self.next_id += 1;
            self.next_id
        };
        self.toasts.retain(|t| t.id != id);
        self.toasts.push(Toast {
            id,
            app: app.into(),
            summary: summary.into(),
            body: body.into(),
            shown: Instant::now(),
            lifetime,
        });
        id
    }
}

type Shared = Arc<Mutex<Store>>;

/// Own org.freedesktop.Notifications if nobody else does; serve it on a thread.
fn serve_dbus(store: Shared) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::new_session() else {
            eprintln!("pancake-notifyd: no D-Bus session; only calendar reminders will show");
            return;
        };
        match conn.request_name("org.freedesktop.Notifications", false, false, true) {
            Ok(dbus::blocking::stdintf::org_freedesktop_dbus::RequestNameReply::PrimaryOwner) => {}
            _ => {
                eprintln!("pancake-notifyd: another notification daemon is running; showing calendar reminders only");
                return;
            }
        }
        let mut cr = Crossroads::new();
        let iface = cr.register("org.freedesktop.Notifications", |b| {
            b.method("GetCapabilities", (), ("capabilities",), |_, _, _: ()| Ok((vec!["body".to_string()],)));
            b.method("GetServerInformation", (), ("name", "vendor", "version", "spec_version"), |_, _, _: ()| {
                Ok(("PancakeDM".to_string(), "PancakeDM".to_string(), env!("CARGO_PKG_VERSION").to_string(), "1.2".to_string()))
            });
            b.method("CloseNotification", ("id",), (), |_, store: &mut Shared, (id,): (u32,)| {
                store.lock().unwrap().toasts.retain(|t| t.id != id);
                Ok(())
            });
            b.method(
                "Notify",
                ("app_name", "replaces_id", "app_icon", "summary", "body", "actions", "hints", "expire_timeout"),
                ("id",),
                |_, store: &mut Shared, (app, rid, _icon, summary, body, _actions, _hints, timeout): (String, u32, String, String, String, Vec<String>, dbus::arg::PropMap, i32)| {
                    let id = store.lock().unwrap().add(rid, &app, &summary, &body, timeout);
                    Ok((id,))
                },
            );
        });
        cr.insert("/org/freedesktop/Notifications", &[iface], store);
        let _ = cr.serve(&conn);
    });
}

struct Win {
    surface: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    bufs: Vec<(wl_buffer::WlBuffer, memmap2::MmapMut)>,
    rows: usize,
}

struct App {
    configured: bool,
    free: [bool; 2],
    pointer: Option<(f64, f64)>,
    clicked: Option<(f64, f64)>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("pancake-notifyd: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let store: Shared = Arc::new(Mutex::new(Store::default()));
    serve_dbus(store.clone());
    let mut fonts = Fonts::load()?;
    let cfg = pancake_config::load();
    let accent = 0xff00_0000 | pancake_draw::accent::effective(&cfg);
    let scale = cfg.scale;
    let top_margin = if cfg.panel_position == PanelPosition::Top { 8 + 44 + 10 } else { 12 };

    let conn = WlConn::connect_to_env().map_err(|e| format!("can't connect to Wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).map_err(|e| e.to_string())?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| e.to_string())?;
    let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).map_err(|_| "no layer shell".to_string())?;
    let _seat: Option<wl_seat::WlSeat> = globals.bind(&qh, 1..=5, ()).ok();

    let mut app = App {
        configured: false,
        free: [true; 2],
        pointer: None,
        clicked: None,
    };
    let mut win: Option<Win> = None;
    let mut events = pancake_config::events::load();
    let mut events_at = Instant::now();
    let mut reminded: HashSet<String> = HashSet::new();
    let mut last_reminder_check = Instant::now() - Duration::from_secs(60);
    let mut drawn_key = String::new();
    let fd = conn.backend().poll_fd().as_raw_fd();

    loop {
        // Calendar reminders.
        if last_reminder_check.elapsed() > Duration::from_secs(15) {
            last_reminder_check = Instant::now();
            if events_at.elapsed() > Duration::from_secs(60) {
                events = pancake_config::events::load();
                events_at = Instant::now();
            }
            eprintln!("pancake-notifyd: checking {} event(s)", events.len());
            for (_, summary, body) in reminders::due(&events, Local::now().naive_local(), &mut reminded) {
                eprintln!("pancake-notifyd: reminder: {summary} ({body})");
                store.lock().unwrap().add(0, "Calendar", &summary, &body, 0);
            }
        }
        // Clicks dismiss.
        if let (Some((x, y)), Some(w)) = (app.clicked.take(), win.as_ref()) {
            let row = (y as u32 / (ROW + GAP)) as usize;
            if x >= 0.0 && (x as u32) < W && row < w.rows {
                let mut s = store.lock().unwrap();
                let shown: Vec<u32> = s.toasts.iter().rev().take(MAX_SHOWN).map(|t| t.id).collect();
                if let Some(id) = shown.get(row) {
                    s.toasts.retain(|t| t.id != *id);
                }
            }
        }
        // Expire.
        store.lock().unwrap().toasts.retain(|t| t.lifetime.map_or(true, |l| t.shown.elapsed() < l));
        let toasts: Vec<Toast> = store.lock().unwrap().toasts.iter().rev().take(MAX_SHOWN).cloned().collect();

        // One surface while there's something to show; recreated when the
        // number of toasts changes (so it never covers more than it should).
        if toasts.is_empty() {
            if let Some(w) = win.take() {
                w.layer.destroy();
                w.surface.destroy();
                drawn_key.clear();
            }
        } else {
            if win.as_ref().map_or(true, |w| w.rows != toasts.len()) {
                if let Some(w) = win.take() {
                    w.layer.destroy();
                    w.surface.destroy();
                }
                let h = toasts.len() as u32 * (ROW + GAP) - GAP;
                let surface = compositor.create_surface(&qh, ());
                let layer = layer_shell.get_layer_surface(&surface, None, zwlr_layer_shell_v1::Layer::Top, "notifications".into(), &qh, ());
                use zwlr_layer_surface_v1::Anchor;
                layer.set_anchor(Anchor::Top | Anchor::Right);
                layer.set_size(W, h);
                layer.set_margin(top_margin, 12, 0, 0);
                layer.set_exclusive_zone(0);
                surface.set_buffer_scale(scale as i32);
                surface.commit();
                app.configured = false;
                app.free = [true; 2];
                drawn_key.clear();
                let (pw, ph) = (W * scale, h * scale);
                let mut bufs = Vec::new();
                for i in 0..2 {
                    let (file, mem) = pancake_draw::shm_file((pw * ph * 4) as u64, &format!("notif-{i}"))?;
                    let pool = shm.create_pool(file.as_fd(), (pw * ph * 4) as i32, &qh, ());
                    let buffer = pool.create_buffer(0, pw as i32, ph as i32, (pw * 4) as i32, wl_shm::Format::Argb8888, &qh, i);
                    pool.destroy();
                    bufs.push((buffer, mem));
                }
                win = Some(Win { surface, layer, bufs, rows: toasts.len() });
            }
            let key: String = toasts.iter().map(|t| format!("{}{}{}", t.id, t.summary, t.body)).collect();
            if let (Some(w), true) = (win.as_mut(), app.configured && key != drawn_key) {
                if let Some(i) = (0..2).find(|&i| app.free[i]) {
                    let h = toasts.len() as u32 * (ROW + GAP) - GAP;
                    let px = pancake_draw::pixels(&mut w.bufs[i].1);
                    let mut c = Canvas::new(px, W as i32, h as i32, scale as f32);
                    draw(&mut c, &mut fonts, &toasts, accent, app.pointer);
                    w.surface.attach(Some(&w.bufs[i].0), 0, 0);
                    w.surface.damage_buffer(0, 0, (W * scale) as i32, (h * scale) as i32);
                    w.surface.commit();
                    app.free[i] = false;
                    drawn_key = key;
                }
            }
        }

        conn.flush().map_err(|e| e.to_string())?;
        let timeout = if toasts.is_empty() { 1000 } else { 150 };
        if let Some(guard) = queue.prepare_read() {
            if pancake_draw::wait_readable(fd, timeout) {
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
}

fn draw(c: &mut Canvas, fonts: &mut Fonts, toasts: &[Toast], accent: u32, pointer: Option<(f64, f64)>) {
    c.clear();
    for (i, t) in toasts.iter().enumerate() {
        let y = (i as u32 * (ROW + GAP)) as f32;
        let r = (0.0, y, W as f32, ROW as f32);
        let hover = pointer.is_some_and(|(px, py)| (py as f32) >= r.1 && (py as f32) < r.1 + r.3 && px >= 0.0 && (px as f32) < r.2);
        // One quiet surface, with an app mark and natural type hierarchy.
        // Avoid the nested cards/accent rails that made the old toast look
        // like a generic generated component.
        c.rounded_rect(r.0, r.1, r.2, r.3, 14.0, 0x30ffffff);
        c.rounded_rect(r.0 + 1.0, r.1 + 1.0, r.2 - 2.0, r.3 - 2.0, 13.0, if hover { 0xfb29231f } else { 0xf91d1916 });
        let (icx, icy) = (r.0 + 30.0, r.1 + 31.0);
        c.circle(icx, icy, 17.0, 0x22ffffff);
        let initial = t.app.chars().find(|ch| ch.is_alphanumeric()).map(|ch| ch.to_uppercase().collect::<String>()).unwrap_or_else(|| "•".into());
        let iw = fonts.width(&initial, 12.0, true);
        c.text(fonts, &initial, icx - iw / 2.0, icy + 4.0, 12.0, true, accent);
        let tx = r.0 + 56.0;
        let tw = W as f32 - tx - 30.0;
        let app = fonts.fit(&t.app, 11.0, true, tw - 38.0);
        c.text(fonts, &app, tx, y + 20.0, 11.0, true, TEXT_DIM);
        let age = if t.shown.elapsed() < Duration::from_secs(60) { "now" } else { "1m" };
        let aw = fonts.width(age, 10.0, false);
        c.text(fonts, age, W as f32 - 14.0 - aw, y + 20.0, 10.0, false, TEXT_FAINT);
        let summary = fonts.fit(&t.summary, 14.0, true, tw);
        c.text(fonts, &summary, tx, y + 42.0, 14.0, true, TEXT);
        let body = fonts.fit(t.body.lines().next().unwrap_or(""), 12.0, false, tw);
        c.text(fonts, &body, tx, y + 64.0, 12.0, false, TEXT_DIM);
        if hover {
            c.circle(W as f32 - 16.0, y + 44.0, 10.0, 0x20ffffff);
            c.text(fonts, "×", W as f32 - 19.5, y + 48.0, 13.0, false, TEXT_DIM);
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for App {
    fn event(app: &mut Self, layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, _: &(), _: &WlConn, _: &QueueHandle<Self>) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            layer.ack_configure(serial);
            app.configured = true;
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for App {
    fn event(app: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, i: &usize, _: &WlConn, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            app.free[*i] = true;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(_: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &WlConn, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(app: &mut Self, _: &wl_pointer::WlPointer, event: wl_pointer::Event, _: &(), _: &WlConn, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => app.pointer = Some((surface_x, surface_y)),
            wl_pointer::Event::Leave { .. } => app.pointer = None,
            wl_pointer::Event::Button {
                state: WEnum::Value(wl_pointer::ButtonState::Pressed), ..
            } => app.clicked = app.pointer,
            _ => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &WlConn, _: &QueueHandle<Self>) {}
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
