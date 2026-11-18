//! Where frames go and input comes from. The compositor loop doesn't care
//! which backend it runs on:
//!   - `drm`: real screens and input devices (from a TTY or display manager)
//!   - `nested`: a window inside another Wayland desktop, for development

pub mod drm;
pub mod nested;

pub use crate::low::{InputEvent, Output};
use crate::render::Canvas;

#[derive(Debug)]
pub enum Event {
    Input(InputEvent),
    /// Frames landed on these outputs (bitmask): they're on screen now.
    FrameDone(u32),
    /// These outputs can take a new frame again, but nothing new was shown.
    Ready(u32),
    /// We lost (false) or got back (true) the screens, e.g. VT switch.
    Active(bool),
    /// Output sizes changed; call `outputs()` again.
    Resized,
    /// The backend wants the session to end (e.g. nested window closed).
    Quit,
}

pub trait Backend {
    fn outputs(&self) -> Vec<Output>;
    /// Fds to wait on; `dispatch` gets one readiness flag per fd, same order.
    fn fds(&mut self) -> Vec<i32>;
    fn dispatch(&mut self, ready: &[bool], events: &mut Vec<Event>);
    /// The buffer to draw output `index`'s next frame into; None = busy.
    fn begin_frame(&mut self, index: u32) -> Result<Option<Canvas<'_>>, String>;
    /// Ok(true) = on screen now; Ok(false) = wait for FrameDone.
    fn commit_frame(&mut self, index: u32) -> Result<bool, String>;
    /// The pointer moved to (x, y) on output 0 (for a hardware cursor).
    fn pointer_moved(&mut self, _x: i32, _y: i32) {}
    /// Ctrl+Alt+F<n>.
    fn switch_vt(&mut self, _n: i32) {}
}
