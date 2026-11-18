//! Bindings to the Zig low-level layer (see `lowlevel/include/pancake_low.h`).

#[cfg(feature = "zig-backend")]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct OutputInfo {
        pub width: u32,
        pub height: u32,
        pub refresh_mhz: u32,
        pub name: [u8; 32],
    }

    extern "C" {
        pub fn pancake_low_version() -> u32;
        pub fn pancake_low_open(path: *const c_char) -> *mut c_void;
        pub fn pancake_low_open_fd(fd: c_int) -> *mut c_void;

        pub fn pancake_seat_open() -> *mut c_void;
        pub fn pancake_seat_name(seat: *mut c_void) -> *const c_char;
        pub fn pancake_seat_fd(seat: *mut c_void) -> c_int;
        pub fn pancake_seat_dispatch(seat: *mut c_void) -> c_int;
        pub fn pancake_seat_ack_disable(seat: *mut c_void);
        pub fn pancake_seat_switch(seat: *mut c_void, session: c_int) -> c_int;
        pub fn pancake_seat_open_device(seat: *mut c_void, path: *const c_char) -> c_int;
        pub fn pancake_seat_close(seat: *mut c_void);
        pub fn pancake_low_output_count(dev: *mut c_void) -> u32;
        pub fn pancake_low_output_info(dev: *mut c_void, index: u32, info: *mut OutputInfo) -> c_int;
        pub fn pancake_low_fd(dev: *mut c_void) -> c_int;
        pub fn pancake_frame_begin(dev: *mut c_void, index: u32, frame: *mut Frame) -> c_int;
        pub fn pancake_frame_commit(dev: *mut c_void, index: u32) -> c_int;
        pub fn pancake_low_dispatch(dev: *mut c_void) -> u32;
        pub fn pancake_low_reset(dev: *mut c_void);
        pub fn pancake_low_close(dev: *mut c_void);

        pub fn pancake_cursor_size(dev: *mut c_void, w: *mut u32, h: *mut u32);
        pub fn pancake_cursor_set(dev: *mut c_void, index: u32, pixels: *const u32, w: u32, h: u32, hot_x: i32, hot_y: i32) -> c_int;
        pub fn pancake_cursor_move(dev: *mut c_void, index: u32, x: i32, y: i32) -> c_int;
        pub fn pancake_cursor_hide(dev: *mut c_void, index: u32) -> c_int;

        pub fn pancake_vt_take() -> c_int;
        pub fn pancake_vt_release();

        pub fn pancake_input_open(seat: *mut c_void) -> *mut c_void;
        pub fn pancake_input_suspend(input: *mut c_void);
        pub fn pancake_input_resume(input: *mut c_void) -> c_int;
        pub fn pancake_input_fd(input: *mut c_void) -> c_int;
        pub fn pancake_input_next(input: *mut c_void, out: *mut InputEvent) -> c_int;
        pub fn pancake_input_close(input: *mut c_void);

        pub fn poll(fds: *mut PollFd, nfds: u64, timeout_ms: c_int) -> c_int;
    }

    #[repr(C)]
    pub struct Frame {
        pub pixels: *mut u32,
        pub stride: u32,
        pub width: u32,
        pub height: u32,
    }

    #[repr(C)]
    pub struct InputEvent {
        pub kind: u32,
        pub code: u32,
        pub pressed: u32,
        pub _pad: u32,
        pub time_usec: u64,
        pub x: f64,
        pub y: f64,
        pub name: [u8; 64],
    }

    #[repr(C)]
    pub struct PollFd {
        pub fd: c_int,
        pub events: i16,
        pub revents: i16,
    }
}

#[cfg(feature = "zig-backend")]
pub fn version() -> String {
    let v = unsafe { ffi::pancake_low_version() };
    format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
}

#[cfg(not(feature = "zig-backend"))]
pub fn version() -> String {
    "not linked (build with --features zig-backend)".into()
}

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
}

/// A GPU opened through the Zig layer. Dropping it restores the screens.
pub struct Device {
    #[cfg(feature = "zig-backend")]
    raw: *mut std::ffi::c_void,
}

#[cfg(feature = "zig-backend")]
impl Device {
    pub fn open(path: Option<&str>) -> Result<Self, String> {
        let cpath = path.map(|p| std::ffi::CString::new(p).map_err(|e| e.to_string())).transpose()?;
        let ptr = cpath.as_ref().map_or(std::ptr::null(), |p| p.as_ptr());
        let raw = unsafe { ffi::pancake_low_open(ptr) };
        if raw.is_null() {
            return Err(format!("can't open {}", path.unwrap_or("/dev/dri/card0")));
        }
        Ok(Self { raw })
    }

    /// Open a GPU through the seat (no `video` group needed).
    pub fn open_via(seat: &Seat, path: &str) -> Result<Self, String> {
        let cpath = std::ffi::CString::new(path).map_err(|e| e.to_string())?;
        let fd = unsafe { ffi::pancake_seat_open_device(seat.raw, cpath.as_ptr()) };
        if fd < 0 {
            return Err(format!("the seat wouldn't give us {path}"));
        }
        let raw = unsafe { ffi::pancake_low_open_fd(fd) };
        if raw.is_null() {
            return Err(format!("can't set up {path}"));
        }
        Ok(Self { raw })
    }

    pub fn outputs(&self) -> Vec<Output> {
        let n = unsafe { ffi::pancake_low_output_count(self.raw) };
        (0..n)
            .filter_map(|i| {
                let mut info = ffi::OutputInfo { width: 0, height: 0, refresh_mhz: 0, name: [0; 32] };
                if unsafe { ffi::pancake_low_output_info(self.raw, i, &mut info) } != 0 {
                    return None;
                }
                let len = info.name.iter().position(|&b| b == 0).unwrap_or(32);
                Some(Output {
                    name: String::from_utf8_lossy(&info.name[..len]).into_owned(),
                    width: info.width,
                    height: info.height,
                    refresh_mhz: info.refresh_mhz,
                })
            })
            .collect()
    }

    /// The DRM fd: wait on it, then call `dispatch`.
    pub fn fd(&self) -> i32 {
        unsafe { ffi::pancake_low_fd(self.raw) }
    }

    /// The hidden buffer to draw output `index`'s next frame into, or None
    /// while its previous frame is still waiting for the GPU to flip.
    pub fn begin_frame(&self, index: u32) -> Result<Option<crate::render::Canvas<'_>>, String> {
        let mut f = ffi::Frame { pixels: std::ptr::null_mut(), stride: 0, width: 0, height: 0 };
        match unsafe { ffi::pancake_frame_begin(self.raw, index, &mut f) } {
            0 => {}
            1 => return Ok(None),
            e => return Err(format!("can't get a frame buffer ({e})")),
        }
        // Safety: the Zig layer hands us a mapped buffer of stride*height
        // pixels that nothing else touches until we commit.
        let pixels = unsafe { std::slice::from_raw_parts_mut(f.pixels, (f.stride * f.height) as usize) };
        Ok(Some(crate::render::Canvas::new(pixels, f.stride, f.width, f.height)))
    }

    /// Show the frame. Ok(true) = on screen now; Ok(false) = flip queued,
    /// the output is busy until `dispatch` reports it.
    pub fn commit_frame(&self, index: u32) -> Result<bool, String> {
        match unsafe { ffi::pancake_frame_commit(self.raw, index) } {
            0 => Ok(false),
            2 => Ok(true),
            -3 => Err("the GPU refused the frame: another compositor owns it (run from a TTY)".into()),
            e => Err(format!("commit failed ({e})")),
        }
    }

    /// Handle DRM events; returns a bitmask of outputs whose flip landed.
    pub fn dispatch(&self) -> u32 {
        unsafe { ffi::pancake_low_dispatch(self.raw) }
    }

    /// After the VT comes back: forget pending flips, re-modeset next frame.
    pub fn reset(&self) {
        unsafe { ffi::pancake_low_reset(self.raw) }
    }

    /// Largest cursor image the GPU's cursor plane takes.
    pub fn cursor_size(&self) -> (u32, u32) {
        let (mut w, mut h) = (0, 0);
        unsafe { ffi::pancake_cursor_size(self.raw, &mut w, &mut h) };
        (w, h)
    }

    pub fn set_cursor(&self, index: u32, img: &crate::cursor::CursorImage) -> Result<(), String> {
        let r = unsafe {
            ffi::pancake_cursor_set(self.raw, index, img.pixels.as_ptr(), img.width, img.height, img.hot_x, img.hot_y)
        };
        match r {
            0 => Ok(()),
            -3 => Err("this GPU has no hardware cursor plane".into()),
            -4 => Err("cursor image is bigger than the GPU's cursor plane".into()),
            e => Err(format!("cursor setup failed ({e})")),
        }
    }

    pub fn move_cursor(&self, index: u32, x: i32, y: i32) {
        unsafe { ffi::pancake_cursor_move(self.raw, index, x, y) };
    }

    pub fn hide_cursor(&self, index: u32) {
        unsafe { ffi::pancake_cursor_hide(self.raw, index) };
    }
}

#[cfg(feature = "zig-backend")]
impl Drop for Device {
    fn drop(&mut self) {
        unsafe { ffi::pancake_low_close(self.raw) }
    }
}

#[cfg(not(feature = "zig-backend"))]
impl Device {
    pub fn open(_path: Option<&str>) -> Result<Self, String> {
        Err("built without the Zig low-level layer (use `make`)".into())
    }
    pub fn open_via(_seat: &Seat, _path: &str) -> Result<Self, String> {
        Err("built without the Zig low-level layer (use `make`)".into())
    }
    pub fn outputs(&self) -> Vec<Output> {
        Vec::new()
    }
    pub fn fd(&self) -> i32 {
        -1
    }
    pub fn begin_frame(&self, _index: u32) -> Result<Option<crate::render::Canvas<'_>>, String> {
        Err("built without the Zig low-level layer".into())
    }
    pub fn commit_frame(&self, _index: u32) -> Result<bool, String> {
        Err("built without the Zig low-level layer".into())
    }
    pub fn dispatch(&self) -> u32 {
        0
    }
    pub fn reset(&self) {}
    pub fn cursor_size(&self) -> (u32, u32) {
        (64, 64)
    }
    pub fn set_cursor(&self, _index: u32, _img: &crate::cursor::CursorImage) -> Result<(), String> {
        Err("built without the Zig low-level layer".into())
    }
    pub fn move_cursor(&self, _index: u32, _x: i32, _y: i32) {}
    pub fn hide_cursor(&self, _index: u32) {}
}

// ---- Input ------------------------------------------------------------------

/// evdev key codes we care about so far.
pub mod key {
    pub const ESC: u32 = 1;
    pub const BACKSPACE: u32 = 14;
    pub const LEFTCTRL: u32 = 29;
    pub const RIGHTCTRL: u32 = 97;
    pub const LEFTALT: u32 = 56;
    pub const RIGHTALT: u32 = 100;

    /// F1..F12 -> 1..12.
    pub fn function_number(code: u32) -> Option<i32> {
        match code {
            59..=68 => Some(code as i32 - 58),
            87 => Some(11),
            88 => Some(12),
            _ => None,
        }
    }
}

#[cfg_attr(not(feature = "zig-backend"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    DeviceAdded(String),
    DeviceRemoved(String),
    Key { code: u32, pressed: bool },
    Motion { dx: f64, dy: f64 },
    /// Position as a 0..1 fraction of the screen (tablets, VMs).
    MotionAbsolute { x: f64, y: f64 },
    Button { code: u32, pressed: bool },
    Scroll { dx: f64, dy: f64 },
}

/// Keyboard/mouse input through the Zig layer (libinput).
pub struct Input {
    #[cfg(feature = "zig-backend")]
    raw: *mut std::ffi::c_void,
}

#[cfg(feature = "zig-backend")]
impl Input {
    /// Through the seat if there is one, otherwise directly.
    pub fn open(seat: Option<&Seat>) -> Result<Self, String> {
        let raw = unsafe { ffi::pancake_input_open(seat.map_or(std::ptr::null_mut(), |s| s.raw)) };
        if raw.is_null() {
            return Err(match seat {
                Some(_) => "can't start input through the seat".into(),
                None => "can't start input (are you in the `input` group?)".into(),
            });
        }
        Ok(Self { raw })
    }

    pub fn fd(&self) -> i32 {
        unsafe { ffi::pancake_input_fd(self.raw) }
    }

    /// Wait up to `timeout_ms` for input; true if some arrived.
    pub fn wait(&self, timeout_ms: i32) -> bool {
        wait(&[self.fd()], timeout_ms)[0]
    }

    /// Let go of input devices (the seat was disabled).
    pub fn suspend(&self) {
        unsafe { ffi::pancake_input_suspend(self.raw) }
    }

    /// Take input devices back (the seat was enabled again).
    pub fn resume(&self) {
        unsafe { ffi::pancake_input_resume(self.raw) };
    }

    pub fn next(&self) -> Option<InputEvent> {
        let mut e = std::mem::MaybeUninit::<ffi::InputEvent>::zeroed();
        if unsafe { ffi::pancake_input_next(self.raw, e.as_mut_ptr()) } == 0 {
            return None;
        }
        let e = unsafe { e.assume_init() };
        let name = || {
            let len = e.name.iter().position(|&b| b == 0).unwrap_or(e.name.len());
            String::from_utf8_lossy(&e.name[..len]).into_owned()
        };
        Some(match e.kind {
            1 => InputEvent::DeviceAdded(name()),
            2 => InputEvent::DeviceRemoved(name()),
            3 => InputEvent::Key { code: e.code, pressed: e.pressed != 0 },
            4 => InputEvent::Motion { dx: e.x, dy: e.y },
            5 => InputEvent::MotionAbsolute { x: e.x, y: e.y },
            6 => InputEvent::Button { code: e.code, pressed: e.pressed != 0 },
            _ => InputEvent::Scroll { dx: e.x, dy: e.y },
        })
    }
}

#[cfg(feature = "zig-backend")]
impl Drop for Input {
    fn drop(&mut self) {
        unsafe { ffi::pancake_input_close(self.raw) }
    }
}

#[cfg(not(feature = "zig-backend"))]
impl Input {
    pub fn open(_seat: Option<&Seat>) -> Result<Self, String> {
        Err("built without the Zig low-level layer (use `make`)".into())
    }
    pub fn fd(&self) -> i32 {
        -1
    }
    pub fn wait(&self, _timeout_ms: i32) -> bool {
        false
    }
    pub fn suspend(&self) {}
    pub fn resume(&self) {}
    pub fn next(&self) -> Option<InputEvent> {
        None
    }
}

// ---- Virtual terminal -------------------------------------------------------

/// Holds the VT while alive: no console text, keys don't reach the shell.
/// Dropping it (or a crash/kill) gives the VT back.
pub struct Vt {
    _private: (),
}

impl Vt {
    /// None when not running on a Linux VT (inside a desktop terminal, or
    /// with no terminal at all, like under a display manager).
    pub fn take() -> Option<Self> {
        #[cfg(feature = "zig-backend")]
        if unsafe { ffi::pancake_vt_take() } == 0 {
            return Some(Self { _private: () });
        }
        None
    }
}

impl Drop for Vt {
    fn drop(&mut self) {
        #[cfg(feature = "zig-backend")]
        unsafe {
            ffi::pancake_vt_release()
        }
    }
}

// ---- Waiting on several fds -------------------------------------------------

/// Wait up to `timeout_ms` until any fd is readable; one bool per fd.
#[cfg(feature = "zig-backend")]
pub fn wait(fds: &[i32], timeout_ms: i32) -> Vec<bool> {
    let mut pfds: Vec<ffi::PollFd> = fds.iter().map(|&fd| ffi::PollFd { fd, events: 1, revents: 0 }).collect();
    if unsafe { ffi::poll(pfds.as_mut_ptr(), pfds.len() as u64, timeout_ms) } <= 0 {
        return vec![false; fds.len()];
    }
    pfds.iter().map(|p| p.revents != 0).collect()
}

#[cfg(not(feature = "zig-backend"))]
pub fn wait(fds: &[i32], timeout_ms: i32) -> Vec<bool> {
    std::thread::sleep(std::time::Duration::from_millis(timeout_ms.max(0) as u64));
    vec![false; fds.len()]
}

// ---- Seat -------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SeatEvents {
    /// The VT is ours again: take devices back and redraw everything.
    pub enabled: bool,
    /// Switched away: stop using devices, then call `ack_disable`.
    pub disabled: bool,
}

/// Seat access through libseat (systemd-logind, elogind or seatd).
/// Dropping it closes every device it opened.
pub struct Seat {
    #[cfg(feature = "zig-backend")]
    raw: *mut std::ffi::c_void,
}

#[cfg(feature = "zig-backend")]
impl Seat {
    /// None when no seat backend is running.
    pub fn open() -> Option<Self> {
        let raw = unsafe { ffi::pancake_seat_open() };
        (!raw.is_null()).then_some(Self { raw })
    }

    pub fn name(&self) -> String {
        let p = unsafe { ffi::pancake_seat_name(self.raw) };
        if p.is_null() {
            return "?".into();
        }
        unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    pub fn fd(&self) -> i32 {
        unsafe { ffi::pancake_seat_fd(self.raw) }
    }

    pub fn dispatch(&self) -> SeatEvents {
        let r = unsafe { ffi::pancake_seat_dispatch(self.raw) };
        if r < 0 {
            return SeatEvents::default();
        }
        SeatEvents { enabled: r & 1 != 0, disabled: r & 2 != 0 }
    }

    pub fn ack_disable(&self) {
        unsafe { ffi::pancake_seat_ack_disable(self.raw) }
    }

    /// Switch to VT/session `n` (Ctrl+Alt+F<n>).
    pub fn switch_to(&self, n: i32) {
        unsafe { ffi::pancake_seat_switch(self.raw, n) };
    }
}

#[cfg(feature = "zig-backend")]
impl Drop for Seat {
    fn drop(&mut self) {
        unsafe { ffi::pancake_seat_close(self.raw) }
    }
}

#[cfg(not(feature = "zig-backend"))]
impl Seat {
    pub fn open() -> Option<Self> {
        None
    }
    pub fn name(&self) -> String {
        String::new()
    }
    pub fn fd(&self) -> i32 {
        -1
    }
    pub fn dispatch(&self) -> SeatEvents {
        SeatEvents::default()
    }
    pub fn ack_disable(&self) {}
    pub fn switch_to(&self, _n: i32) {}
}
