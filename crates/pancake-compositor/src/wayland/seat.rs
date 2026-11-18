//! Input for apps: keyboard focus with an xkb keymap, pointer focus,
//! clicks, scrolling, and moving windows (Alt+drag, or the app asking).

use super::{same_client, State};
use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use wayland_server::protocol::{wl_keyboard, wl_pointer, wl_surface::WlSurface};
use wayland_server::Resource;
use xkbcommon::xkb;

/// evdev left mouse button.
pub const BTN_LEFT: u32 = 0x110;

/// The keymap apps get, plus our copy of the modifier state.
pub struct Keyboard {
    file: File,
    size: u32,
    state: xkb::State,
    pressed: Vec<u32>,
}

impl Keyboard {
    /// Empty names let libxkbcommon use XKB_DEFAULT_LAYOUT and friends,
    /// or the system default, so the user's layout setting is respected.
    pub fn new() -> Result<Self, String> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(&ctx, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .ok_or("can't load a keyboard layout (is xkeyboard-config installed?)")?;
        let text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR isn't set")?;
        let path = std::path::Path::new(&dir).join(format!("pancake-keymap-{}", std::process::id()));
        let mut file = File::options().read(true).write(true).create(true).truncate(true).open(&path).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&path);
        file.write_all(text.as_bytes()).and_then(|_| file.write_all(&[0])).map_err(|e| e.to_string())?;
        Ok(Self { file, size: text.len() as u32 + 1, state: xkb::State::new(&keymap), pressed: Vec::new() })
    }

    fn mods(&self) -> (u32, u32, u32, u32) {
        (
            self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        )
    }

    fn pressed_bytes(&self) -> Vec<u8> {
        self.pressed.iter().flat_map(|k| k.to_ne_bytes()).collect()
    }
}

/// A window being dragged: keep the pointer at the same spot on it.
pub struct Grab {
    surface: WlSurface,
    dx: i32,
    dy: i32,
}

impl State {
    pub(super) fn time_ms(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    /// Bring a window (and any popups) to the front.
    pub(super) fn raise(&mut self, i: usize) {
        if i + 1 != self.windows.len() {
            let w = self.windows.remove(i);
            self.windows.push(w);
            // Popups stay above every window.
            let (popups, rest): (Vec<_>, Vec<_>) = self.windows.drain(..).partition(|w| w.popup.is_some());
            self.windows = rest;
            self.windows.extend(popups);
            self.dirty = true;
        }
    }

    // ---- keyboard ----------------------------------------------------------

    /// Send the keymap to a newly created wl_keyboard, and enter if its
    /// client already has focus.
    pub(super) fn add_keyboard(&mut self, kb: wl_keyboard::WlKeyboard) {
        let Some(k) = &self.keyboard else { return };
        if kb.version() >= 4 {
            kb.repeat_info(25, 600);
        }
        kb.keymap(wl_keyboard::KeymapFormat::XkbV1, k.file.as_fd(), k.size);
        if let Some(focus) = self.kb_focus.clone() {
            if same_client(&kb, &focus) {
                let serial = self.next_serial();
                let k = self.keyboard.as_ref().unwrap();
                kb.enter(serial, &focus, k.pressed_bytes());
                let (d, l, lk, g) = k.mods();
                kb.modifiers(serial, d, l, lk, g);
            }
        }
        self.keyboards.push(kb);
    }

    pub fn set_keyboard_focus(&mut self, surface: Option<WlSurface>) {
        if self.kb_focus == surface {
            return;
        }
        // Tell the old and new windows whether they're active.
        let old_root = self.kb_focus.as_ref().and_then(|s| self.root_window(s));
        let new_root = surface.as_ref().and_then(|s| self.root_window(s));
        if old_root != new_root {
            if let Some(i) = old_root {
                self.send_activated(i, false);
            }
            if let Some(i) = new_root {
                self.send_activated(i, true);
            }
        }
        self.keyboards.retain(|k| k.is_alive());
        let serial = self.next_serial();
        if let Some(old) = self.kb_focus.take() {
            for kb in self.keyboards.iter().filter(|k| same_client(*k, &old)) {
                kb.leave(serial, &old);
            }
        }
        if let (Some(new), Some(k)) = (&surface, &self.keyboard) {
            let (d, l, lk, g) = k.mods();
            for kb in self.keyboards.iter().filter(|kb| same_client(*kb, new)) {
                kb.enter(serial, new, k.pressed_bytes());
                kb.modifiers(serial, d, l, lk, g);
            }
        }
        self.kb_focus = surface;
        if let Some(i) = new_root {
            let ws = self.windows[i].workspace as usize;
            self.last_focus[ws] = self.windows[i].id;
        }
        self.send_selection();
    }

    /// A key the compositor didn't keep for itself.
    pub fn key(&mut self, code: u32, pressed: bool) {
        let Some(k) = &mut self.keyboard else { return };
        let dir = if pressed { xkb::KeyDirection::Down } else { xkb::KeyDirection::Up };
        k.state.update_key(xkb::Keycode::new(code + 8), dir);
        if pressed {
            if !k.pressed.contains(&code) {
                k.pressed.push(code);
            }
        } else {
            k.pressed.retain(|&c| c != code);
        }
        let mods = k.mods();
        let Some(focus) = self.kb_focus.clone() else { return };
        let (serial, time) = (self.next_serial(), self.time_ms());
        let state = if pressed { wl_keyboard::KeyState::Pressed } else { wl_keyboard::KeyState::Released };
        for kb in self.keyboards.iter().filter(|kb| kb.is_alive() && same_client(*kb, &focus)) {
            kb.key(serial, time, code, state);
            kb.modifiers(serial, mods.0, mods.1, mods.2, mods.3);
        }
    }

    // ---- pointer -----------------------------------------------------------

    fn pointers_for<'a>(&'a self, surface: &'a WlSurface) -> impl Iterator<Item = &'a wl_pointer::WlPointer> {
        self.pointers.iter().filter(move |p| p.is_alive() && same_client(*p, surface))
    }

    fn pointer_frame(&self, surface: &WlSurface) {
        for p in self.pointers_for(surface).filter(|p| p.version() >= 5) {
            p.frame();
        }
    }

    pub fn pointer_motion(&mut self, x: f64, y: f64) {
        self.pointer_pos = (x, y);
        if let Some(g) = &self.grab {
            if let Some(w) = self.windows.iter_mut().find(|w| w.surface == g.surface) {
                w.x = x as i32 - g.dx;
                w.y = y as i32 - g.dy;
                self.dirty = true;
            }
            return;
        }
        let target = self.hit(x, y).map(|(_, s, sx, sy)| (s, sx, sy));
        let time = self.time_ms();
        match target {
            Some((surface, sx, sy)) if Some(&surface) == self.pointer_focus.as_ref() => {
                for p in self.pointers_for(&surface) {
                    p.motion(time, sx, sy);
                }
                self.pointer_frame(&surface);
            }
            target => {
                let serial = self.next_serial();
                if let Some(old) = self.pointer_focus.take() {
                    for p in self.pointers_for(&old) {
                        p.leave(serial, &old);
                    }
                    self.pointer_frame(&old);
                }
                if let Some((surface, sx, sy)) = target {
                    for p in self.pointers_for(&surface) {
                        p.enter(serial, &surface, sx, sy);
                    }
                    self.pointer_frame(&surface);
                    self.pointer_focus = Some(surface);
                }
            }
        }
    }

    /// `alt`: Alt is held, so a left-drag moves the window instead of
    /// going to the app.
    pub fn pointer_button(&mut self, code: u32, pressed: bool, alt: bool) {
        if pressed {
            self.buttons_down += 1;
        } else {
            self.buttons_down = self.buttons_down.saturating_sub(1);
        }
        if self.grab.is_some() {
            if self.buttons_down == 0 {
                self.grab = None;
                let (x, y) = self.pointer_pos;
                self.pointer_motion(x, y); // re-find what's under the pointer
            }
            return;
        }
        if pressed {
            let (x, y) = self.pointer_pos;
            let hit = self.hit(x, y).and_then(|(i, ..)| i);
            // A click outside the open menus closes them.
            if hit.map_or(true, |i| self.windows[i].popup.is_none()) {
                self.dismiss_popups();
            }
            // Click to focus and raise (toplevels; menus keep their own focus).
            if let Some(i) = hit.filter(|&i| self.windows[i].toplevel.is_some()) {
                let surface = self.windows[i].surface.clone();
                if self.windows[i].floating {
                    self.raise(i);
                }
                let top = self.windows.iter().position(|w| w.surface == surface).unwrap();
                self.set_keyboard_focus(Some(surface));
                if alt && code == BTN_LEFT {
                    self.start_move(top);
                    return;
                }
            }
        }
        let Some(focus) = self.pointer_focus.clone() else { return };
        let (serial, time) = (self.next_serial(), self.time_ms());
        let state = if pressed { wl_pointer::ButtonState::Pressed } else { wl_pointer::ButtonState::Released };
        for p in self.pointers_for(&focus) {
            p.button(serial, time, code, state);
        }
        self.pointer_frame(&focus);
    }

    pub fn pointer_axis(&mut self, dx: f64, dy: f64) {
        let Some(focus) = self.pointer_focus.clone() else { return };
        let time = self.time_ms();
        for p in self.pointers_for(&focus) {
            if dy != 0.0 {
                p.axis(time, wl_pointer::Axis::VerticalScroll, dy);
            }
            if dx != 0.0 {
                p.axis(time, wl_pointer::Axis::HorizontalScroll, dx);
            }
        }
        self.pointer_frame(&focus);
    }

    /// Windows appeared, moved or went away: re-check what's under the
    /// pointer so the right app gets enter/leave without waiting for motion.
    pub(super) fn refresh_pointer_focus(&mut self) {
        if self.grab.is_none() {
            let (x, y) = self.pointer_pos;
            self.pointer_motion(x, y);
        }
    }

    /// Start dragging window `i` with the pointer.
    pub(super) fn start_move(&mut self, i: usize) {
        if self.buttons_down == 0 || self.windows[i].toplevel.is_none() {
            return; // only toplevels, only while a button is held
        }
        // Dragging a tiled window pops it out to float, same size, same spot.
        if !self.windows[i].floating {
            let frame = self.frame_rect(i);
            let (ox, oy) = self.origin(i);
            let w = &mut self.windows[i];
            w.floating = true;
            w.maximized = false;
            w.float_size = Some((frame.2, frame.3));
            w.x = ox;
            w.y = oy;
            self.relayout();
        }
        let (x, y) = self.pointer_pos;
        let w = &self.windows[i];
        self.grab = Some(Grab { surface: w.surface.clone(), dx: x as i32 - w.x, dy: y as i32 - w.y });
        // The app shouldn't see the drag as pointer input.
        if let Some(old) = self.pointer_focus.take() {
            let serial = self.next_serial();
            for p in self.pointers_for(&old) {
                p.leave(serial, &old);
            }
            self.pointer_frame(&old);
        }
    }

    /// A surface went away: drop any focus or grab pointing at it, and give
    /// the keyboard to the window now on top.
    pub(super) fn forget_surface(&mut self, surface: &WlSurface) {
        if self.pointer_focus.as_ref() == Some(surface) {
            self.pointer_focus = None;
        }
        if self.grab.as_ref().is_some_and(|g| g.surface == *surface) {
            self.grab = None;
        }
        if self.kb_focus.as_ref() == Some(surface) {
            self.kb_focus = None;
            let top = self.windows.iter().rev().find(|w| w.placed && w.toplevel.is_some() && w.surface != *surface).map(|w| w.surface.clone());
            self.set_keyboard_focus(top);
        }
        self.refresh_pointer_focus();
    }
}
