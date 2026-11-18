//! pancake-hello: a tiny Wayland app for testing PancakeDM.
//!
//! Opens a window using only wl_shm + xdg_shell, and redraws on every
//! frame callback with a moving progress bar, so you can see that the
//! compositor shows windows and paces their frames.

use std::fs::File;
use std::os::fd::AsFd;
use std::time::Instant;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const W: u32 = 480;
const H: u32 = 300;

struct App {
    configured: bool,
    frame_due: bool,
    buffer_free: bool,
    running: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("pancake-hello: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("can't connect to Wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();

    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).map_err(|e| format!("wl_compositor: {e}"))?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("wl_shm: {e}"))?;
    let wm_base: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=3, ()).map_err(|e| format!("xdg_wm_base: {e}"))?;

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("Pancake Hello".into());
    toplevel.set_app_id("org.pancakedm.Hello".into());
    surface.commit();

    // Shared memory for one buffer. We can reuse it once the compositor
    // releases it (PancakeDM copies on commit, so that's right away).
    let size = (W * H * 4) as u64;
    let file = shm_file(size)?;
    let mut mem = unsafe { memmap2::MmapMut::map_mut(&file) }.map_err(|e| e.to_string())?;
    let pool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
    let buffer = pool.create_buffer(0, W as i32, H as i32, (W * 4) as i32, wl_shm::Format::Argb8888, &qh, ());

    let mut app = App { configured: false, frame_due: true, buffer_free: true, running: true };
    let start = Instant::now();
    let mut frames = 0u64;
    while app.running {
        queue.blocking_dispatch(&mut app).map_err(|e| e.to_string())?;
        if app.configured && app.frame_due && app.buffer_free {
            draw(&mut mem, start.elapsed().as_secs_f32());
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, W as i32, H as i32);
            surface.frame(&qh, ());
            surface.commit();
            app.frame_due = false;
            app.buffer_free = false;
            frames += 1;
            if frames % 120 == 0 {
                let fps = frames as f32 / start.elapsed().as_secs_f32();
                eprintln!("pancake-hello: {frames} frames, {fps:.1} fps");
            }
        }
    }
    Ok(())
}

/// An unlinked file in XDG_RUNTIME_DIR to share pixels through.
fn shm_file(size: u64) -> Result<File, String> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR isn't set")?;
    let path = std::path::Path::new(&dir).join(format!("pancake-hello-{}", std::process::id()));
    let file = File::options().read(true).write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);
    file.set_len(size).map_err(|e| e.to_string())?;
    Ok(file)
}

/// A rounded card with a header strip and a progress bar that loops.
fn draw(mem: &mut [u8], t: f32) {
    const RADIUS: f32 = 18.0;
    const CARD: u32 = 0xfffff3e3;
    const HEADER: u32 = 0xff5b8e7d;
    const TRACK: u32 = 0xffe8d9c4;
    const BAR: u32 = 0xffbc4b51;
    let progress = (t / 3.0).fract();
    let (bar_x0, bar_x1, bar_y0, bar_y1) = (40.0, W as f32 - 40.0, 190.0, 214.0);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut px = if fy < 64.0 { HEADER } else { CARD };
            if (bar_y0..bar_y1).contains(&fy) && (bar_x0..bar_x1).contains(&fx) {
                let filled = bar_x0 + (bar_x1 - bar_x0) * progress;
                px = if fx < filled { BAR } else { TRACK };
            }
            // Rounded corners with a soft 1px edge, premultiplied.
            let cx = fx.clamp(RADIUS, W as f32 - RADIUS);
            let cy = fy.clamp(RADIUS, H as f32 - RADIUS);
            let dist = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let cover = (RADIUS + 0.5 - dist).clamp(0.0, 1.0);
            let px = premultiply(px, cover);
            let i = ((y * W + x) * 4) as usize;
            mem[i..i + 4].copy_from_slice(&px.to_le_bytes());
        }
    }
}

fn premultiply(argb: u32, cover: f32) -> u32 {
    if cover >= 1.0 {
        return argb;
    }
    let a = ((argb >> 24) as f32 * cover) as u32;
    let ch = |s: u32| ((((argb >> s) & 0xff) as f32 * cover) as u32) << s;
    (a << 24) | ch(16) | ch(8) | ch(0)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
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

impl Dispatch<wl_callback::WlCallback, ()> for App {
    fn event(app: &mut Self, _: &wl_callback::WlCallback, event: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            app.frame_due = true;
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(app: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            app.buffer_free = true;
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
