//! The Wayland server: what apps talk to.
//!
//! Protocol plumbing comes from the `wayland-server` and `wayland-protocols`
//! crates (message encoding and the socket); everything the compositor
//! actually does with windows is ours.
//!
//! Supported so far: wl_compositor, wl_shm, xdg_wm_base (toplevel windows),
//! plus basic wl_output and wl_seat so common clients start up.

mod seat;
mod shm;

use seat::{Grab, Keyboard};
use shm::{BufferData, Pool};
use std::os::fd::{AsFd, AsRawFd};
use std::sync::{Arc, Mutex};
use wayland_protocols::xdg::shell::server::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_server::backend::{ClientData, ClientId};
use wayland_server::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback::{self, WlCallback},
    wl_compositor::{self, WlCompositor},
    wl_keyboard::{self, WlKeyboard},
    wl_output::{self, WlOutput},
    wl_pointer::{self, WlPointer},
    wl_region::{self, WlRegion},
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::{self, WlShmPool},
    wl_surface::{self, WlSurface},
    wl_touch::{self, WlTouch},
};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, ListeningSocket, New, Resource,
};

use crate::render::Canvas;

/// The listening socket plus the protocol state machine.
pub struct Server {
    display: Display<State>,
    socket: ListeningSocket,
    pub name: String,
}

impl Server {
    pub fn new() -> Result<Self, String> {
        let display = Display::<State>::new().map_err(|e| format!("can't start Wayland: {e:?}"))?;
        let dh = display.handle();
        dh.create_global::<State, WlCompositor, ()>(5, ());
        dh.create_global::<State, WlShm, ()>(1, ());
        dh.create_global::<State, WlOutput, ()>(3, ());
        dh.create_global::<State, WlSeat, ()>(5, ());
        dh.create_global::<State, XdgWmBase, ()>(3, ());
        let socket = ListeningSocket::bind_auto("wayland", 1..33)
            .map_err(|e| format!("can't create the Wayland socket: {e:?}"))?;
        let name = socket.socket_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self { display, socket, name })
    }

    /// Readable when connected clients sent requests.
    pub fn display_fd(&mut self) -> i32 {
        self.display.backend().poll_fd().as_raw_fd()
    }

    /// Readable when a new client is connecting.
    pub fn socket_fd(&self) -> i32 {
        self.socket.as_fd().as_raw_fd()
    }

    pub fn accept(&mut self) {
        while let Ok(Some(stream)) = self.socket.accept() {
            if let Err(e) = self.display.handle().insert_client(stream, Arc::new(ClientState)) {
                eprintln!("pancake-compositor: dropping a client: {e}");
            }
        }
    }

    pub fn dispatch(&mut self, state: &mut State) {
        if let Err(e) = self.display.dispatch_clients(state) {
            eprintln!("pancake-compositor: Wayland dispatch error: {e}");
        }
    }

    pub fn flush(&mut self) {
        let _ = self.display.flush_clients();
    }
}

struct ClientState;
impl ClientData for ClientState {}

/// A toplevel app window.
struct Window {
    surface: WlSurface,
    toplevel: XdgToplevel,
    title: String,
    x: i32,
    y: i32,
    placed: bool,
}

/// Everything the protocol handlers read and change.
pub struct State {
    windows: Vec<Window>,
    /// Frame callbacks from commits not drawn yet.
    frame_callbacks: Vec<WlCallback>,
    /// Frame callbacks whose content is in a frame waiting to be shown.
    in_flight: Vec<WlCallback>,
    /// Something changed on screen; the compositor should draw a new frame.
    pub dirty: bool,
    output: (u32, u32, u32), // width, height, refresh in mHz
    serial: u32,
    placed_count: i32,
    started: std::time::Instant,
    // Input (see seat.rs).
    keyboard: Option<Keyboard>,
    keyboards: Vec<WlKeyboard>,
    pointers: Vec<WlPointer>,
    kb_focus: Option<WlSurface>,
    pointer_focus: Option<WlSurface>,
    pointer_pos: (f64, f64),
    buttons_down: u32,
    grab: Option<Grab>,
}

impl State {
    pub fn new(width: u32, height: u32, refresh_mhz: u32) -> Self {
        Self {
            windows: Vec::new(),
            frame_callbacks: Vec::new(),
            in_flight: Vec::new(),
            dirty: false,
            output: (width, height, refresh_mhz),
            serial: 0,
            placed_count: 0,
            started: std::time::Instant::now(),
            keyboard: Keyboard::new()
                .map_err(|e| eprintln!("pancake-compositor: {e}; apps get no keyboard"))
                .ok(),
            keyboards: Vec::new(),
            pointers: Vec::new(),
            kb_focus: None,
            pointer_focus: None,
            pointer_pos: (0.0, 0.0),
            buttons_down: 0,
            grab: None,
        }
    }

    fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    /// Draw every window, back to front. The frame callbacks of what we
    /// drew now ride along with this frame until it's on screen.
    pub fn draw(&mut self, canvas: &mut Canvas) {
        self.in_flight.append(&mut self.frame_callbacks);
        for w in &self.windows {
            let Some(data) = w.surface.data::<Mutex<SurfaceState>>() else { continue };
            let s = data.lock().unwrap();
            if s.width > 0 {
                canvas.blit(w.x, w.y, s.width, s.height, &s.pixels, s.opaque);
            }
        }
    }

    /// The frame we drew is on screen: tell those apps to draw their next one.
    /// (Commits that arrived after we drew wait for the following frame.)
    pub fn frame_done(&mut self, time_ms: u32) {
        for cb in self.in_flight.drain(..) {
            cb.done(time_ms);
        }
    }

    pub fn window_titles(&self) -> Vec<String> {
        self.windows.iter().map(|w| w.title.clone()).collect()
    }

    pub fn set_output_size(&mut self, width: u32, height: u32) {
        self.output.0 = width;
        self.output.1 = height;
    }

    /// First time a window has content: center it, cascading later ones.
    fn place_window(&mut self, surface: &WlSurface) {
        let (ow, oh, _) = self.output;
        let Some(i) = self.windows.iter().position(|w| w.surface == *surface && !w.placed) else { return };
        let (w, h) = {
            let s = surface.data::<Mutex<SurfaceState>>().unwrap().lock().unwrap();
            if s.width == 0 {
                return;
            }
            (s.width as i32, s.height as i32)
        };
        let step = 32 * (self.placed_count % 8);
        let win = &mut self.windows[i];
        win.x = ((ow as i32 - w) / 2).max(0) + step;
        win.y = ((oh as i32 - h) / 2).max(0) + step;
        win.placed = true;
        self.placed_count += 1;
        // A new window gets the keyboard.
        let surface = win.surface.clone();
        self.set_keyboard_focus(Some(surface));
        self.refresh_pointer_focus();
    }
}

// ---- wl_compositor / wl_surface ----------------------------------------------

/// Per-surface state. Pixels are copied out of the client's buffer on
/// commit and the buffer is released right away, so clients never wait on us.
#[derive(Default)]
pub struct SurfaceState {
    /// Some(Some) = new buffer attached, Some(None) = detached, None = unchanged.
    pending_buffer: Option<Option<WlBuffer>>,
    pending_frames: Vec<WlCallback>,
    pixels: Vec<u32>,
    width: u32,
    height: u32,
    opaque: bool,
}

impl GlobalDispatch<WlCompositor, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WlCompositor>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}

impl Dispatch<WlCompositor, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &WlCompositor, request: wl_compositor::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                init.init(id, Mutex::new(SurfaceState::default()));
            }
            wl_compositor::Request::CreateRegion { id } => {
                init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSurface, Mutex<SurfaceState>> for State {
    fn request(state: &mut Self, _: &Client, surface: &WlSurface, request: wl_surface::Request, data: &Mutex<SurfaceState>, _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_surface::Request::Attach { buffer, .. } => {
                data.lock().unwrap().pending_buffer = Some(buffer);
            }
            wl_surface::Request::Frame { callback } => {
                let cb = init.init(callback, ());
                data.lock().unwrap().pending_frames.push(cb);
            }
            wl_surface::Request::Commit => {
                let mut s = data.lock().unwrap();
                match s.pending_buffer.take() {
                    Some(Some(buf)) => {
                        if let Some(b) = buf.data::<BufferData>() {
                            if let Some(px) = b.read() {
                                s.pixels = px;
                                s.width = b.width;
                                s.height = b.height;
                                s.opaque = b.opaque;
                            }
                        }
                        buf.release();
                    }
                    Some(None) => {
                        s.pixels.clear();
                        s.width = 0;
                        s.height = 0;
                    }
                    None => {}
                }
                state.frame_callbacks.append(&mut s.pending_frames);
                drop(s);
                state.dirty = true;
                state.place_window(surface);
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, surface: &WlSurface, _: &Mutex<SurfaceState>) {
        state.windows.retain(|w| w.surface != *surface);
        state.forget_surface(surface);
        state.dirty = true;
    }
}

impl Dispatch<WlRegion, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &WlRegion, _: wl_region::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl Dispatch<WlCallback, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &WlCallback, _: wl_callback::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

// ---- wl_shm ------------------------------------------------------------------

impl GlobalDispatch<WlShm, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WlShm>, _: &(), init: &mut DataInit<'_, Self>) {
        let shm = init.init(resource, ());
        shm.format(wl_shm::Format::Argb8888);
        shm.format(wl_shm::Format::Xrgb8888);
    }
}

impl Dispatch<WlShm, ()> for State {
    fn request(_: &mut Self, _: &Client, shm: &WlShm, request: wl_shm::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wl_shm::Request::CreatePool { id, fd, size } = request {
            let pool = Pool::new(fd, size);
            if !pool.is_mapped() {
                shm.post_error(wl_shm::Error::InvalidFd, "can't map that pool");
            }
            init.init(id, Arc::new(pool));
        }
    }
}

impl Dispatch<WlShmPool, Arc<Pool>> for State {
    fn request(_: &mut Self, _: &Client, _: &WlShmPool, request: wl_shm_pool::Request, pool: &Arc<Pool>, _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_shm_pool::Request::CreateBuffer { id, offset, width, height, stride, format } => {
                init.init(id, BufferData::new(pool.clone(), offset, width, height, stride, format));
            }
            wl_shm_pool::Request::Resize { size } => pool.resize(size),
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, BufferData> for State {
    fn request(_: &mut Self, _: &Client, _: &WlBuffer, _: wl_buffer::Request, _: &BufferData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

// ---- wl_output / wl_seat -------------------------------------------------------

impl GlobalDispatch<WlOutput, ()> for State {
    fn bind(state: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WlOutput>, _: &(), init: &mut DataInit<'_, Self>) {
        let out = init.init(resource, ());
        let (w, h, refresh) = state.output;
        out.geometry(0, 0, 0, 0, wl_output::Subpixel::Unknown, "PancakeDM".into(), "Screen".into(), wl_output::Transform::Normal);
        out.mode(wl_output::Mode::Current | wl_output::Mode::Preferred, w as i32, h as i32, refresh as i32);
        if out.version() >= 2 {
            out.scale(1);
            out.done();
        }
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &WlOutput, _: wl_output::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl GlobalDispatch<WlSeat, ()> for State {
    fn bind(state: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WlSeat>, _: &(), init: &mut DataInit<'_, Self>) {
        let seat = init.init(resource, ());
        let mut caps = wl_seat::Capability::Pointer;
        if state.keyboard.is_some() {
            caps |= wl_seat::Capability::Keyboard;
        }
        seat.capabilities(caps);
        if seat.version() >= 2 {
            seat.name("seat0".into());
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn request(state: &mut Self, _: &Client, _: &WlSeat, request: wl_seat::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_seat::Request::GetPointer { id } => {
                let p = init.init(id, ());
                state.pointers.push(p);
            }
            wl_seat::Request::GetKeyboard { id } => {
                let kb = init.init(id, ());
                state.add_keyboard(kb);
            }
            wl_seat::Request::GetTouch { id } => {
                init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &WlPointer, _: wl_pointer::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}
impl Dispatch<WlKeyboard, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &WlKeyboard, _: wl_keyboard::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}
impl Dispatch<WlTouch, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &WlTouch, _: wl_touch::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

// ---- xdg_shell ---------------------------------------------------------------

/// The wl_surface an xdg_surface / toplevel / popup belongs to.
pub struct XdgData {
    surface: WlSurface,
}

impl GlobalDispatch<XdgWmBase, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<XdgWmBase>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &XdgWmBase, request: xdg_wm_base::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                init.init(id, XdgData { surface });
            }
            xdg_wm_base::Request::CreatePositioner { id } => {
                init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgPositioner, ()> for State {
    fn request(_: &mut Self, _: &Client, _: &XdgPositioner, _: xdg_positioner::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl Dispatch<XdgSurface, XdgData> for State {
    fn request(state: &mut Self, _: &Client, xdg: &XdgSurface, request: xdg_surface::Request, data: &XdgData, _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            xdg_surface::Request::GetToplevel { id } => {
                let toplevel = init.init(id, XdgData { surface: data.surface.clone() });
                state.windows.push(Window {
                    surface: data.surface.clone(),
                    toplevel: toplevel.clone(),
                    title: String::new(),
                    x: 0,
                    y: 0,
                    placed: false,
                });
                // Let the app pick its own size for now (0x0), then map.
                toplevel.configure(0, 0, Vec::new());
                let serial = state.next_serial();
                xdg.configure(serial);
            }
            xdg_surface::Request::GetPopup { id, .. } => {
                // Popups (menus) aren't shown yet.
                init.init(id, XdgData { surface: data.surface.clone() });
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgToplevel, XdgData> for State {
    fn request(state: &mut Self, _: &Client, toplevel: &XdgToplevel, request: xdg_toplevel::Request, _: &XdgData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        match request {
            xdg_toplevel::Request::SetTitle { title } => {
                if let Some(w) = state.windows.iter_mut().find(|w| w.toplevel == *toplevel) {
                    w.title = title;
                }
            }
            // The app's own title bar was dragged.
            xdg_toplevel::Request::Move { .. } => {
                if let Some(i) = state.windows.iter().position(|w| w.toplevel == *toplevel) {
                    state.start_move(i);
                }
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, toplevel: &XdgToplevel, _: &XdgData) {
        state.windows.retain(|w| w.toplevel != *toplevel);
        state.dirty = true;
    }
}

impl Dispatch<XdgPopup, XdgData> for State {
    fn request(_: &mut Self, _: &Client, _: &XdgPopup, _: xdg_popup::Request, _: &XdgData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}
