//! Run as a window inside another Wayland desktop. For development: real
//! apps can then connect to PancakeDM without a spare TTY or a VM.

use super::{Backend, Event, InputEvent, Output};
use crate::render::Canvas;
use memmap2::MmapMut;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const DEFAULT_SIZE: (u32, u32) = (1280, 800);

struct Buf {
    buffer: wl_buffer::WlBuffer,
    mem: MmapMut,
    size: (u32, u32),
    _file: File,
}

/// What the host compositor's events change.
struct HostState {
    configured: bool,
    size: (u32, u32),
    pending_size: Option<(u32, u32)>,
    busy: [bool; 2],
    events: Vec<Event>,
}

pub struct NestedBackend {
    conn: Connection,
    queue: EventQueue<HostState>,
    qh: QueueHandle<HostState>,
    st: HostState,
    surface: wl_surface::WlSurface,
    shm: wl_shm::WlShm,
    bufs: Vec<Buf>,
    current: usize,
    fd: i32,
}

impl NestedBackend {
    pub fn new() -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("can't connect to the host desktop: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<HostState>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).map_err(|e| e.to_string())?;
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| e.to_string())?;
        let wm_base: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=3, ()).map_err(|e| e.to_string())?;
        if let Ok(seat) = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 1..=5, ()) {
            let _ = seat; // capabilities event creates pointer/keyboard
        }
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title("PancakeDM".into());
        toplevel.set_app_id("org.pancakedm.Nested".into());
        surface.commit();

        let mut st = HostState { configured: false, size: DEFAULT_SIZE, pending_size: None, busy: [false; 2], events: Vec::new() };
        queue.roundtrip(&mut st).map_err(|e| e.to_string())?;
        let fd = conn.backend().poll_fd().as_raw_fd();
        Ok(Self { conn, queue, qh, st, surface, shm, bufs: Vec::new(), current: 0, fd })
    }

    fn make_buf(&self, i: usize, (w, h): (u32, u32)) -> Result<Buf, String> {
        let size = (w * h * 4) as u64;
        let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR isn't set")?;
        let path = std::path::Path::new(&dir).join(format!("pancake-nested-{}-{i}", std::process::id()));
        let file = File::options().read(true).write(true).create(true).truncate(true).open(&path).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&path);
        file.set_len(size).map_err(|e| e.to_string())?;
        let mem = unsafe { MmapMut::map_mut(&file) }.map_err(|e| e.to_string())?;
        let pool = self.shm.create_pool(file.as_fd(), size as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Xrgb8888, &self.qh, i);
        pool.destroy();
        Ok(Buf { buffer, mem, size: (w, h), _file: file })
    }
}

impl Backend for NestedBackend {
    fn outputs(&self) -> Vec<Output> {
        vec![Output { name: "nested".into(), width: self.st.size.0, height: self.st.size.1, refresh_mhz: 60_000 }]
    }

    fn fds(&mut self) -> Vec<i32> {
        let _ = self.conn.flush();
        vec![self.fd]
    }

    fn dispatch(&mut self, ready: &[bool], events: &mut Vec<Event>) {
        if ready[0] {
            if let Some(guard) = self.queue.prepare_read() {
                let _ = guard.read();
            }
        }
        if let Err(e) = self.queue.dispatch_pending(&mut self.st) {
            eprintln!("pancake-compositor: lost the host desktop: {e}");
            self.st.events.push(Event::Quit);
        }
        events.append(&mut self.st.events);
    }

    fn begin_frame(&mut self, _index: u32) -> Result<Option<Canvas<'_>>, String> {
        if !self.st.configured {
            return Ok(None);
        }
        // New size: start over with fresh buffers.
        if self.bufs.first().map_or(true, |b| b.size != self.st.size) {
            for b in self.bufs.drain(..) {
                b.buffer.destroy();
            }
            self.bufs = vec![self.make_buf(0, self.st.size)?, self.make_buf(1, self.st.size)?];
            self.st.busy = [false; 2];
        }
        let Some(i) = (0..2).find(|&i| !self.st.busy[i]) else { return Ok(None) };
        self.current = i;
        let (w, h) = self.bufs[i].size;
        let mem = &mut self.bufs[i].mem;
        // Safety: the mapping is w*h*4 bytes, page-aligned, and ours until commit.
        let px = unsafe { std::slice::from_raw_parts_mut(mem.as_mut_ptr() as *mut u32, (w * h) as usize) };
        Ok(Some(Canvas::new(px, w, w, h)))
    }

    fn commit_frame(&mut self, _index: u32) -> Result<bool, String> {
        let b = &self.bufs[self.current];
        self.surface.attach(Some(&b.buffer), 0, 0);
        self.surface.damage_buffer(0, 0, b.size.0 as i32, b.size.1 as i32);
        self.surface.frame(&self.qh, ());
        self.surface.commit();
        self.st.busy[self.current] = true;
        let _ = self.conn.flush();
        Ok(false)
    }
}

impl HostState {
    fn input(&mut self, e: InputEvent) {
        self.events.push(Event::Input(e));
    }
    fn fraction(&self, x: f64, y: f64) -> InputEvent {
        InputEvent::MotionAbsolute { x: x / self.size.0 as f64, y: y / self.size.1 as f64 }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for HostState {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for HostState {
    fn event(_: &mut Self, base: &xdg_wm_base::XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for HostState {
    fn event(st: &mut Self, xdg: &xdg_surface::XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            if let Some(size) = st.pending_size.take() {
                if size != st.size {
                    st.size = size;
                    st.events.push(Event::Resized);
                }
            }
            st.configured = true;
            st.events.push(Event::Ready(1));
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for HostState {
    fn event(st: &mut Self, _: &xdg_toplevel::XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                st.pending_size = Some((width as u32, height as u32));
            }
            xdg_toplevel::Event::Close => st.events.push(Event::Quit),
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for HostState {
    fn event(st: &mut Self, _: &wl_callback::WlCallback, event: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            st.events.push(Event::FrameDone(1));
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for HostState {
    fn event(st: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            st.busy[*i] = false;
            st.events.push(Event::Ready(1));
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for HostState {
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

impl Dispatch<wl_pointer::WlPointer, ()> for HostState {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                let e = st.fraction(surface_x, surface_y);
                st.input(e);
            }
            wl_pointer::Event::Button { button, state: WEnum::Value(s), .. } => {
                st.input(InputEvent::Button { code: button, pressed: s == wl_pointer::ButtonState::Pressed });
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(axis), value, .. } => {
                let (dx, dy) = if axis == wl_pointer::Axis::HorizontalScroll { (value, 0.0) } else { (0.0, value) };
                st.input(InputEvent::Scroll { dx, dy });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for HostState {
    fn event(st: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_keyboard::Event::Key { key, state: WEnum::Value(s), .. } = event {
            st.input(InputEvent::Key { code: key, pressed: s == wl_keyboard::KeyState::Pressed });
        }
    }
}

delegate_noop!(HostState: ignore wl_compositor::WlCompositor);
delegate_noop!(HostState: ignore wl_surface::WlSurface);
delegate_noop!(HostState: ignore wl_shm::WlShm);
delegate_noop!(HostState: ignore wl_shm_pool::WlShmPool);
