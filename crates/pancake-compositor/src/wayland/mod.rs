//! The Wayland server: what apps talk to.
//!
//! Protocol plumbing comes from the `wayland-server` and `wayland-protocols`
//! crates (message encoding and the socket); everything the compositor
//! actually does with windows is ours.
//!
//! Supported: wl_compositor, wl_subcompositor, wl_shm, wl_output, wl_seat
//! (pointer + keyboard), wl_data_device_manager (copy/paste), xdg_wm_base
//! (windows and popups).

mod data;
mod layer;
mod seat;
mod shm;
mod subsurface;
mod xdg;

use seat::{Grab, Keyboard};
use shm::{BufferData, Pool};
use layer::Layer;
use subsurface::Sub;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::{Arc, Mutex};
use wayland_protocols::xdg::shell::server::{xdg_popup::XdgPopup, xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase};
use wayland_server::protocol::{wl_data_device::WlDataDevice, wl_data_device_manager::WlDataDeviceManager, wl_data_source::WlDataSource, wl_subcompositor::WlSubcompositor};
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
        dh.create_global::<State, WlSubcompositor, ()>(1, ());
        dh.create_global::<State, WlDataDeviceManager, ()>(3, ());
        dh.create_global::<State, WlShm, ()>(1, ());
        dh.create_global::<State, WlOutput, ()>(3, ());
        dh.create_global::<State, WlSeat, ()>(5, ());
        dh.create_global::<State, XdgWmBase, ()>(3, ());
        dh.create_global::<State, wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1, ()>(4, ());
        let socket = ListeningSocket::bind_auto("wayland", 1..33)
            .map_err(|e| format!("can't create the Wayland socket: {e:?}"))?;
        let name = socket.socket_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self { display, socket, name })
    }

    pub fn handle(&self) -> DisplayHandle {
        self.display.handle()
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

/// Two resources belong to the same app.
fn same_client(a: &impl Resource, b: &impl Resource) -> bool {
    a.id().same_client_as(&b.id())
}

/// An app window (toplevel) or menu (popup).
struct Window {
    /// Stable id for the panel and the control socket.
    id: u32,
    surface: WlSurface,
    xdg: XdgSurface,
    toplevel: Option<XdgToplevel>,
    popup: Option<XdgPopup>,
    /// Popups: the surface they hang off; x, y are then relative to the
    /// parent's window geometry. Toplevels: x, y are screen coordinates
    /// of the surface's top-left.
    parent: Option<WlSurface>,
    title: String,
    app_id: String,
    x: i32,
    y: i32,
    /// The "real" window inside the surface, without drop shadows
    /// (xdg_surface.set_window_geometry).
    geometry: Option<(i32, i32, i32, i32)>,
    placed: bool,
}

impl Window {
    fn new(surface: WlSurface, xdg: XdgSurface, toplevel: Option<XdgToplevel>, popup: Option<XdgPopup>, parent: Option<WlSurface>) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { id, surface, xdg, toplevel, popup, parent, title: String::new(), app_id: String::new(), x: 0, y: 0, geometry: None, placed: false }
    }
}

fn surface_size(surface: &WlSurface) -> (i32, i32) {
    surface
        .data::<Mutex<SurfaceState>>()
        .map(|d| {
            let s = d.lock().unwrap();
            (s.width as i32, s.height as i32)
        })
        .unwrap_or((0, 0))
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
    subsurfaces: Vec<Sub>,
    layers: Vec<Layer>,
    /// Screen area not reserved by panels: x, y, w, h.
    usable: (i32, i32, i32, i32),
    // Clipboard (see data.rs).
    dh: Option<DisplayHandle>,
    data_devices: Vec<WlDataDevice>,
    selection: Option<WlDataSource>,
}

impl State {
    pub fn new(dh: DisplayHandle, width: u32, height: u32, refresh_mhz: u32) -> Self {
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
            subsurfaces: Vec::new(),
            layers: Vec::new(),
            usable: (0, 0, width as i32, height as i32),
            dh: Some(dh),
            data_devices: Vec::new(),
            selection: None,
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
        for layer in [layer::BACKGROUND, layer::BOTTOM] {
            for l in self.layers_in(layer) {
                self.draw_tree(canvas, &l.surface, l.rect.0, l.rect.1, 0);
            }
        }
        for i in 0..self.windows.len() {
            if self.windows[i].placed {
                let (x, y) = self.origin(i);
                self.draw_tree(canvas, &self.windows[i].surface, x, y, 0);
            }
        }
        for layer in [layer::TOP, layer::OVERLAY] {
            for l in self.layers_in(layer) {
                self.draw_tree(canvas, &l.surface, l.rect.0, l.rect.1, 0);
            }
        }
    }

    /// A surface and its subsurfaces, with the surface's top-left at (x, y).
    fn draw_tree(&self, canvas: &mut Canvas, surface: &WlSurface, x: i32, y: i32, depth: u32) {
        if depth > 8 {
            return;
        }
        for sub in self.subsurfaces.iter().filter(|s| s.parent == *surface && !s.above) {
            self.draw_tree(canvas, &sub.surface, x + sub.x, y + sub.y, depth + 1);
        }
        if let Some(data) = surface.data::<Mutex<SurfaceState>>() {
            let s = data.lock().unwrap();
            if s.width > 0 {
                canvas.blit(x, y, s.width, s.height, &s.pixels, s.opaque);
            }
        }
        for sub in self.subsurfaces.iter().filter(|s| s.parent == *surface && s.above) {
            self.draw_tree(canvas, &sub.surface, x + sub.x, y + sub.y, depth + 1);
        }
    }

    /// Topmost surface in a tree under (px, py), with coordinates local to it.
    fn hit_tree(&self, surface: &WlSurface, x: i32, y: i32, px: f64, py: f64, depth: u32) -> Option<(WlSurface, f64, f64)> {
        if depth > 8 {
            return None;
        }
        let children: Vec<&Sub> = self.subsurfaces.iter().filter(|s| s.parent == *surface).collect();
        for sub in children.iter().rev().filter(|s| s.above) {
            if let Some(hit) = self.hit_tree(&sub.surface, x + sub.x, y + sub.y, px, py, depth + 1) {
                return Some(hit);
            }
        }
        let (w, h) = surface_size(surface);
        let (lx, ly) = (px - x as f64, py - y as f64);
        if w > 0 && lx >= 0.0 && ly >= 0.0 && lx < w as f64 && ly < h as f64 {
            return Some((surface.clone(), lx, ly));
        }
        for sub in children.iter().rev().filter(|s| !s.above) {
            if let Some(hit) = self.hit_tree(&sub.surface, x + sub.x, y + sub.y, px, py, depth + 1) {
                return Some(hit);
            }
        }
        None
    }

    /// Topmost surface under (x, y), in drawing order reversed: overlay and
    /// top layers, windows, then bottom and background layers. Returns the
    /// window index if it's a window, the exact surface hit (maybe a
    /// subsurface), and coordinates local to that surface.
    fn hit(&self, x: f64, y: f64) -> Option<(Option<usize>, WlSurface, f64, f64)> {
        let layer_hit = |layer: u32| {
            let ls: Vec<&Layer> = self.layers_in(layer).collect();
            ls.into_iter().rev().find_map(|l| self.hit_tree(&l.surface, l.rect.0, l.rect.1, x, y, 0)).map(|(s, lx, ly)| (None, s, lx, ly))
        };
        layer_hit(layer::OVERLAY)
            .or_else(|| layer_hit(layer::TOP))
            .or_else(|| {
                (0..self.windows.len()).rev().filter(|&i| self.windows[i].placed).find_map(|i| {
                    let (ox, oy) = self.origin(i);
                    self.hit_tree(&self.windows[i].surface, ox, oy, x, y, 0).map(|(s, lx, ly)| (Some(i), s, lx, ly))
                })
            })
            .or_else(|| layer_hit(layer::BOTTOM))
            .or_else(|| layer_hit(layer::BACKGROUND))
    }

    /// Offset of the window geometry inside the surface (shadow size).
    fn geom_offset(&self, i: usize) -> (i32, i32) {
        self.windows[i].geometry.map_or((0, 0), |(x, y, _, _)| (x, y))
    }

    /// Screen position of window `i`'s surface top-left. Popups follow
    /// their parent, so they move with it.
    fn origin(&self, i: usize) -> (i32, i32) {
        let mut idx = i;
        let (mut x, mut y) = (0, 0);
        for _ in 0..16 {
            let w = &self.windows[idx];
            let Some(p) = w.parent.as_ref().and_then(|p| self.windows.iter().position(|o| o.surface == *p)) else {
                return (x + w.x, y + w.y);
            };
            // Popup: relative to the parent's geometry, minus our own shadow.
            let (gx, gy) = self.geom_offset(p);
            let (sx, sy) = self.geom_offset(idx);
            x += w.x + gx - sx;
            y += w.y + gy - sy;
            idx = p;
        }
        (x, y)
    }

    /// The frame we drew is on screen: tell those apps to draw their next one.
    /// (Commits that arrived after we drew wait for the following frame.)
    pub fn frame_done(&mut self, time_ms: u32) {
        for cb in self.in_flight.drain(..) {
            cb.done(time_ms);
        }
    }

    /// Open windows, oldest first.
    pub fn window_list(&self) -> Vec<pancake_ipc::WindowInfo> {
        let focused = self.kb_focus.as_ref().and_then(|s| self.root_window(s));
        let mut list: Vec<pancake_ipc::WindowInfo> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.toplevel.is_some() && w.placed)
            .map(|(i, w)| pancake_ipc::WindowInfo { id: w.id, title: w.title.clone(), focused: Some(i) == focused, app_id: w.app_id.clone() })
            .collect();
        list.sort_by_key(|w| w.id);
        list
    }

    /// Raise and focus a window by id (the panel's window list).
    pub fn focus_window(&mut self, id: u32) -> bool {
        let Some(i) = self.windows.iter().position(|w| w.id == id && w.toplevel.is_some()) else { return false };
        let surface = self.windows[i].surface.clone();
        self.raise(i);
        self.set_keyboard_focus(Some(surface));
        true
    }

    pub fn set_output_size(&mut self, width: u32, height: u32) {
        self.output.0 = width;
        self.output.1 = height;
        self.arrange_layers();
    }

    /// First time a window has content: center it, cascading later ones.
    /// Centering uses the window geometry, so drop shadows don't count.
    fn place_window(&mut self, surface: &WlSurface) {
        let (ow, oh, _) = self.output;
        let Some(i) = self.windows.iter().position(|w| w.surface == *surface && !w.placed && w.toplevel.is_some()) else { return };
        let (w, h) = surface_size(surface);
        if w == 0 {
            return;
        }
        let (gx, gy, gw, gh) = self.windows[i].geometry.unwrap_or((0, 0, w, h));
        let step = 32 * (self.placed_count % 8);
        // Center in the space panels leave free.
        let (ux, uy, uw, uh) = self.usable;
        let _ = (ow, oh);
        let win = &mut self.windows[i];
        win.x = ux + ((uw - gw) / 2).max(0) - gx + step;
        win.y = uy + ((uh - gh) / 2).max(0) - gy + step;
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
                state.layer_commit(surface);
                state.place_window(surface);
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, surface: &WlSurface, _: &Mutex<SurfaceState>) {
        state.windows.retain(|w| w.surface != *surface);
        state.subsurfaces.retain(|s| s.surface != *surface && s.parent != *surface);
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

