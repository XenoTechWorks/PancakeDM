//! Real screens (DRM/KMS) and input devices (libinput), through the seat.

use super::{Backend, Event, Output};
use crate::cursor::CursorImage;
use crate::low;
use crate::render::Canvas;

pub struct DrmBackend {
    // Field order is drop order: input and screens are released first,
    // then the VT, then the seat.
    input: low::Input,
    dev: low::Device,
    _vt: Option<low::Vt>,
    seat: Option<low::Seat>,
    outputs: Vec<Output>,
    cursor: CursorImage,
    has_cursor: bool,
    /// Set up the cursor after the next modeset (start, VT return).
    cursor_pending: bool,
    pointer: (i32, i32),
    active: bool,
}

impl DrmBackend {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let seat = low::Seat::open();
        let vt = if seat.is_none() { low::Vt::take() } else { None };
        let path = device.unwrap_or("/dev/dri/card0");
        let dev = match &seat {
            Some(s) => {
                eprintln!("pancake-compositor: using seat {}", s.name());
                low::Device::open_via(s, path)?
            }
            None => {
                eprintln!("pancake-compositor: no seat backend (logind/elogind/seatd); opening devices directly");
                low::Device::open(Some(path))?
            }
        };
        let outputs = dev.outputs();
        if outputs.is_empty() {
            return Err("no connected screens found".into());
        }
        let input = low::Input::open(seat.as_ref())?;
        let cursor = CursorImage::placeholder_for(outputs[0].height, dev.cursor_size());
        let pointer = (outputs[0].width as i32 / 2, outputs[0].height as i32 / 2);
        Ok(Self {
            input,
            dev,
            _vt: vt,
            seat,
            outputs,
            cursor,
            has_cursor: false,
            cursor_pending: true,
            pointer,
            active: true,
        })
    }

    fn setup_cursor(&mut self) {
        for i in 1..self.outputs.len() as u32 {
            self.dev.hide_cursor(i);
        }
        self.has_cursor = match self.dev.set_cursor(0, &self.cursor) {
            Ok(()) => {
                self.dev.move_cursor(0, self.pointer.0, self.pointer.1);
                true
            }
            Err(e) => {
                eprintln!("pancake-compositor: {e}; running without a cursor");
                false
            }
        };
    }
}

impl Backend for DrmBackend {
    fn outputs(&self) -> Vec<Output> {
        self.outputs.clone()
    }

    fn fds(&mut self) -> Vec<i32> {
        let mut fds = vec![self.input.fd(), self.dev.fd()];
        if let Some(s) = &self.seat {
            fds.push(s.fd());
        }
        fds
    }

    fn dispatch(&mut self, ready: &[bool], events: &mut Vec<Event>) {
        if ready[1] {
            let landed = self.dev.dispatch();
            if landed != 0 {
                events.push(Event::FrameDone(landed));
            }
        }
        if let (Some(s), Some(true)) = (&self.seat, ready.get(2)) {
            let ev = s.dispatch();
            if ev.disabled {
                // Switched away: let go of everything, then tell the seat.
                self.active = false;
                self.input.suspend();
                s.ack_disable();
                events.push(Event::Active(false));
            }
            if ev.enabled {
                self.active = true;
                self.input.resume();
                self.dev.reset();
                self.cursor_pending = true;
                events.push(Event::Active(true));
            }
        }
        if self.active && ready[0] {
            while let Some(e) = self.input.next() {
                events.push(Event::Input(e));
            }
        }
    }

    fn begin_frame(&mut self, index: u32) -> Result<Option<Canvas<'_>>, String> {
        self.dev.begin_frame(index)
    }

    fn commit_frame(&mut self, index: u32) -> Result<bool, String> {
        let shown_now = self.dev.commit_frame(index)?;
        // After the modeset, so the cursor sits on our screen.
        if shown_now && index == 0 && self.cursor_pending {
            self.cursor_pending = false;
            self.setup_cursor();
        }
        Ok(shown_now)
    }

    fn pointer_moved(&mut self, x: i32, y: i32) {
        self.pointer = (x, y);
        if self.has_cursor {
            self.dev.move_cursor(0, x, y);
        }
    }

    fn switch_vt(&mut self, n: i32) {
        if let Some(s) = &self.seat {
            s.switch_to(n);
        }
    }
}
