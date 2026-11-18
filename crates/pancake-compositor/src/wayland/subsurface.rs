//! wl_subcompositor: surfaces attached to other surfaces (title bars,
//! borders, video). Drawn relative to their parent, above or below it.
//!
//! Simplification for now: subsurface state applies on its own commit
//! (as if every subsurface were in desync mode).

use super::State;
use wayland_server::backend::ClientId;
use wayland_server::protocol::{
    wl_subcompositor::{self, WlSubcompositor},
    wl_subsurface::{self, WlSubsurface},
    wl_surface::WlSurface,
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New};

pub struct Sub {
    pub surface: WlSurface,
    pub parent: WlSurface,
    pub x: i32,
    pub y: i32,
    /// Drawn above the parent (default) or below it.
    pub above: bool,
}

pub struct SubData {
    surface: WlSurface,
}

impl GlobalDispatch<WlSubcompositor, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WlSubcompositor>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}

impl Dispatch<WlSubcompositor, ()> for State {
    fn request(state: &mut Self, _: &Client, _: &WlSubcompositor, request: wl_subcompositor::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wl_subcompositor::Request::GetSubsurface { id, surface, parent } = request {
            init.init(id, SubData { surface: surface.clone() });
            state.subsurfaces.push(Sub { surface, parent, x: 0, y: 0, above: true });
            state.dirty = true;
        }
    }
}

impl Dispatch<WlSubsurface, SubData> for State {
    fn request(state: &mut Self, _: &Client, _: &WlSubsurface, request: wl_subsurface::Request, data: &SubData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let Some(i) = state.subsurfaces.iter().position(|s| s.surface == data.surface) else { return };
        match request {
            wl_subsurface::Request::SetPosition { x, y } => {
                state.subsurfaces[i].x = x;
                state.subsurfaces[i].y = y;
            }
            wl_subsurface::Request::PlaceAbove { sibling } => state.restack(i, &sibling, true),
            wl_subsurface::Request::PlaceBelow { sibling } => state.restack(i, &sibling, false),
            _ => return,
        }
        state.dirty = true;
    }

    fn destroyed(state: &mut Self, _: ClientId, _: &WlSubsurface, data: &SubData) {
        state.subsurfaces.retain(|s| s.surface != data.surface);
        state.dirty = true;
    }
}

impl State {
    /// Move subsurface `i` just above/below `sibling` (or its parent).
    fn restack(&mut self, i: usize, sibling: &WlSurface, above: bool) {
        let mut sub = self.subsurfaces.remove(i);
        if *sibling == sub.parent {
            sub.above = above;
            // Closest to the parent: first of the above-list, last of the below-list.
            let pos = if above { 0 } else { self.subsurfaces.len() };
            self.subsurfaces.insert(pos, sub);
        } else if let Some(j) = self.subsurfaces.iter().position(|s| s.surface == *sibling) {
            sub.above = self.subsurfaces[j].above;
            self.subsurfaces.insert(if above { j + 1 } else { j }, sub);
        } else {
            self.subsurfaces.insert(i.min(self.subsurfaces.len()), sub);
        }
    }
}
