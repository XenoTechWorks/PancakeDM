//! xdg_shell: app windows (toplevels), menus (popups), window geometry,
//! and telling the focused window it's active.

use super::{State, Window};
use std::sync::Mutex;
use wayland_protocols::xdg::shell::server::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_server::backend::ClientId;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum};

/// The wl_surface an xdg_surface / toplevel / popup belongs to.
pub struct XdgData {
    pub surface: WlSurface,
}

/// Where a popup goes, as described by the app (all relative to the
/// parent's window geometry).
#[derive(Default, Clone, Copy)]
pub struct Positioner {
    size: (i32, i32),
    anchor_rect: (i32, i32, i32, i32),
    anchor: u32,
    gravity: u32,
    offset: (i32, i32),
}

impl Positioner {
    /// The popup's rectangle relative to the parent's window geometry.
    /// Anchor/gravity numbering: 1 top, 2 bottom, 3 left, 4 right,
    /// 5 top-left, 6 bottom-left, 7 top-right, 8 bottom-right, else center.
    pub fn place(&self) -> (i32, i32, i32, i32) {
        let (ax, ay, aw, ah) = self.anchor_rect;
        let (px, py) = match self.anchor {
            1 => (ax + aw / 2, ay),
            2 => (ax + aw / 2, ay + ah),
            3 => (ax, ay + ah / 2),
            4 => (ax + aw, ay + ah / 2),
            5 => (ax, ay),
            6 => (ax, ay + ah),
            7 => (ax + aw, ay),
            8 => (ax + aw, ay + ah),
            _ => (ax + aw / 2, ay + ah / 2),
        };
        let (w, h) = self.size;
        let x = match self.gravity {
            3 | 5 | 6 => px - w,
            4 | 7 | 8 => px,
            _ => px - w / 2,
        };
        let y = match self.gravity {
            1 | 5 | 7 => py - h,
            2 | 6 | 8 => py,
            _ => py - h / 2,
        };
        (x + self.offset.0, y + self.offset.1, w, h)
    }
}

impl State {
    /// Index of the window a surface belongs to, following popups up to
    /// their toplevel.
    pub(super) fn root_window(&self, surface: &WlSurface) -> Option<usize> {
        let mut i = self.windows.iter().position(|w| w.surface == *surface)?;
        for _ in 0..16 {
            match &self.windows[i].parent {
                Some(p) => i = self.windows.iter().position(|w| w.surface == *p)?,
                None => return Some(i),
            }
        }
        None
    }

    /// Tell a toplevel whether it's the active (focused) window, so apps
    /// can draw focused and unfocused title bars.
    pub(super) fn send_activated(&mut self, i: usize, active: bool) {
        self.windows[i].activated = active;
        self.send_configure(i);
    }

    /// Position a popup relative to its parent, kept on screen.
    fn popup_rect(&self, parent: Option<&WlSurface>, pos: &Positioner) -> (i32, i32, i32, i32) {
        let (x, y, w, h) = pos.place();
        let Some(p) = parent.and_then(|p| self.windows.iter().position(|w| w.surface == *p)) else { return (x, y, w, h) };
        let (ox, oy) = self.origin(p);
        let (gx, gy) = self.geom_offset(p);
        let (bx, by) = (ox + gx, oy + gy);
        let (ow, oh) = (self.output.0 as i32, self.output.1 as i32);
        let sx = (bx + x).min(ow - w).max(0);
        let sy = (by + y).min(oh - h).max(0);
        (sx - bx, sy - by, w, h)
    }

    /// Close every open popup (a click landed somewhere else).
    pub(super) fn dismiss_popups(&mut self) {
        for w in &self.windows {
            if let Some(p) = &w.popup {
                p.popup_done();
            }
        }
    }
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
                init.init(id, Mutex::new(Positioner::default()));
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgPositioner, Mutex<Positioner>> for State {
    fn request(_: &mut Self, _: &Client, _: &XdgPositioner, request: xdg_positioner::Request, data: &Mutex<Positioner>, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let mut p = data.lock().unwrap();
        match request {
            xdg_positioner::Request::SetSize { width, height } => p.size = (width, height),
            xdg_positioner::Request::SetAnchorRect { x, y, width, height } => p.anchor_rect = (x, y, width, height),
            xdg_positioner::Request::SetAnchor { anchor: WEnum::Value(a) } => p.anchor = a as u32,
            xdg_positioner::Request::SetGravity { gravity: WEnum::Value(g) } => p.gravity = g as u32,
            xdg_positioner::Request::SetOffset { x, y } => p.offset = (x, y),
            _ => {}
        }
    }
}

impl Dispatch<XdgSurface, XdgData> for State {
    fn request(state: &mut Self, _: &Client, xdg: &XdgSurface, request: xdg_surface::Request, data: &XdgData, _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            xdg_surface::Request::GetToplevel { id } => {
                let toplevel = init.init(id, XdgData { surface: data.surface.clone() });
                let mut win = Window::new(data.surface.clone(), xdg.clone(), Some(toplevel), None, None);
                win.workspace = state.active_ws;
                win.floating = state.style.new_windows_float;
                win.order = state.next_order;
                state.next_order += 1;
                state.windows.push(win);
                // Tiled: the first configure already carries its tile size.
                state.relayout();
            }
            xdg_surface::Request::GetPopup { id, parent, positioner } => {
                let parent = parent.and_then(|p| p.data::<XdgData>().map(|d| d.surface.clone()));
                let pos = *positioner.data::<Mutex<Positioner>>().unwrap().lock().unwrap();
                let (x, y, w, h) = state.popup_rect(parent.as_ref(), &pos);
                let popup = init.init(id, XdgData { surface: data.surface.clone() });
                let mut win = Window::new(data.surface.clone(), xdg.clone(), None, Some(popup.clone()), parent);
                win.x = x;
                win.y = y;
                win.placed = true;
                state.windows.push(win);
                popup.configure(x, y, w, h);
                let serial = state.next_serial();
                xdg.configure(serial);
                state.dirty = true;
            }
            xdg_surface::Request::SetWindowGeometry { x, y, width, height } => {
                if let Some(w) = state.windows.iter_mut().find(|w| w.surface == data.surface) {
                    w.geometry = Some((x, y, width, height));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgToplevel, XdgData> for State {
    fn request(state: &mut Self, _: &Client, toplevel: &XdgToplevel, request: xdg_toplevel::Request, _: &XdgData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let find = |state: &State| state.windows.iter().position(|w| w.toplevel.as_ref() == Some(toplevel));
        match request {
            xdg_toplevel::Request::SetTitle { title } => {
                if let Some(i) = find(state) {
                    state.windows[i].title = title;
                }
            }
            xdg_toplevel::Request::SetMinSize { width, height } => {
                if let Some(i) = find(state) {
                    state.windows[i].min_size = (width, height);
                }
            }
            xdg_toplevel::Request::SetMaxSize { width, height } => {
                if let Some(i) = find(state) {
                    state.windows[i].max_size = (width, height);
                }
            }
            xdg_toplevel::Request::SetParent { parent } => {
                if let Some(i) = find(state) {
                    state.windows[i].has_parent = parent.is_some();
                }
            }
            xdg_toplevel::Request::SetMaximized => {
                if let Some(i) = find(state) {
                    state.windows[i].maximized = true;
                    state.relayout();
                }
            }
            xdg_toplevel::Request::UnsetMaximized => {
                if let Some(i) = find(state) {
                    state.windows[i].maximized = false;
                    state.relayout();
                }
            }
            xdg_toplevel::Request::SetAppId { app_id } => {
                if let Some(i) = find(state) {
                    state.windows[i].app_id = app_id;
                }
            }
            // The app's own title bar was dragged.
            xdg_toplevel::Request::Move { .. } => {
                if let Some(i) = find(state) {
                    state.start_move(i);
                }
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, toplevel: &XdgToplevel, _: &XdgData) {
        if let Some(i) = state.windows.iter().position(|w| w.toplevel.as_ref() == Some(toplevel)) {
            let surface = state.windows.remove(i).surface;
            state.forget_surface(&surface);
            state.relayout();
        }
        state.dirty = true;
    }
}

impl Dispatch<XdgPopup, XdgData> for State {
    fn request(state: &mut Self, _: &Client, popup: &XdgPopup, request: xdg_popup::Request, data: &XdgData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        match request {
            // Menus take the keyboard while open.
            xdg_popup::Request::Grab { .. } => state.set_keyboard_focus(Some(data.surface.clone())),
            xdg_popup::Request::Reposition { positioner, token } => {
                let pos = *positioner.data::<Mutex<Positioner>>().unwrap().lock().unwrap();
                let Some(i) = state.windows.iter().position(|w| w.popup.as_ref() == Some(popup)) else { return };
                let parent = state.windows[i].parent.clone();
                let (x, y, w, h) = state.popup_rect(parent.as_ref(), &pos);
                state.windows[i].x = x;
                state.windows[i].y = y;
                popup.repositioned(token);
                popup.configure(x, y, w, h);
                let serial = state.next_serial();
                state.windows[i].xdg.configure(serial);
                state.dirty = true;
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, popup: &XdgPopup, _: &XdgData) {
        if let Some(i) = state.windows.iter().position(|w| w.popup.as_ref() == Some(popup)) {
            let w = state.windows.remove(i);
            // Give the keyboard back to the window the menu came from.
            if state.kb_focus.as_ref() == Some(&w.surface) {
                state.kb_focus = None;
                let back = w.parent.as_ref().and_then(|p| state.root_window(p)).map(|r| state.windows[r].surface.clone());
                state.set_keyboard_focus(back);
            }
            state.forget_surface(&w.surface);
        }
        state.dirty = true;
    }
}
