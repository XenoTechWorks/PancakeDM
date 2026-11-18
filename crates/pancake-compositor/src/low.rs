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
        pub fn pancake_low_output_count(dev: *mut c_void) -> u32;
        pub fn pancake_low_output_info(dev: *mut c_void, index: u32, info: *mut OutputInfo) -> c_int;
        pub fn pancake_low_fill(dev: *mut c_void, index: u32, xrgb: u32) -> c_int;
        pub fn pancake_low_close(dev: *mut c_void);

        pub fn pancake_input_open(seat: *const c_char) -> *mut c_void;
        pub fn pancake_input_fd(input: *mut c_void) -> c_int;
        pub fn pancake_input_next(input: *mut c_void, out: *mut InputEvent) -> c_int;
        pub fn pancake_input_close(input: *mut c_void);

        pub fn poll(fds: *mut PollFd, nfds: u64, timeout_ms: c_int) -> c_int;
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

    pub fn fill(&self, index: u32, xrgb: u32) -> Result<(), String> {
        match unsafe { ffi::pancake_low_fill(self.raw, index, xrgb) } {
            0 => Ok(()),
            -3 => Err("modeset failed: another compositor owns the GPU (run from a TTY)".into()),
            e => Err(format!("fill failed ({e})")),
        }
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
    pub fn outputs(&self) -> Vec<Output> {
        Vec::new()
    }
    pub fn fill(&self, _index: u32, _xrgb: u32) -> Result<(), String> {
        Err("built without the Zig low-level layer".into())
    }
}

// ---- Input ------------------------------------------------------------------

/// evdev key codes we care about so far.
pub mod key {
    pub const ESC: u32 = 1;
    pub const SPACE: u32 = 57;
}

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
    pub fn open() -> Result<Self, String> {
        let raw = unsafe { ffi::pancake_input_open(std::ptr::null()) };
        if raw.is_null() {
            return Err("can't start input (are you in the `input` group?)".into());
        }
        Ok(Self { raw })
    }

    /// Wait up to `timeout_ms` for input; true if some arrived.
    pub fn wait(&self, timeout_ms: i32) -> bool {
        let mut pfd = ffi::PollFd { fd: unsafe { ffi::pancake_input_fd(self.raw) }, events: 1, revents: 0 };
        unsafe { ffi::poll(&mut pfd, 1, timeout_ms) > 0 }
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
    pub fn open() -> Result<Self, String> {
        Err("built without the Zig low-level layer (use `make`)".into())
    }
    pub fn wait(&self, _timeout_ms: i32) -> bool {
        false
    }
    pub fn next(&self) -> Option<InputEvent> {
        None
    }
}
