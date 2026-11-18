//! wlr-layer-shell: panels, docks, wallpapers and overlays. Layer surfaces
//! are anchored to screen edges instead of being free windows, and can
//! reserve space ("exclusive zone") so windows don't open underneath.

use super::State;
use wayland_protocols_wlr::layer_shell::v1::server::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};
use wayland_server::backend::ClientId;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, WEnum};

pub const BACKGROUND: u32 = 0;
pub const BOTTOM: u32 = 1;
pub const TOP: u32 = 2;
pub const OVERLAY: u32 = 3;

const ANCHOR_TOP: u32 = 1;
const ANCHOR_BOTTOM: u32 = 2;
const ANCHOR_LEFT: u32 = 4;
const ANCHOR_RIGHT: u32 = 8;

pub struct Layer {
    pub surface: WlSurface,
    resource: ZwlrLayerSurfaceV1,
    pub layer: u32,
    anchor: u32,
    size: (u32, u32),
    zone: i32,
    margin: (i32, i32, i32, i32), // top, right, bottom, left
    configured: bool,
    /// Where it is on screen, once arranged.
    pub rect: (i32, i32, i32, i32),
}

pub struct LayerData {
    surface: WlSurface,
}

impl State {
    /// Lay out every layer surface and work out the space left for windows.
    /// Sends a configure to any whose size changed (or that never had one).
    pub(super) fn arrange_layers(&mut self) {
        let (ow, oh) = (self.output.0 as i32, self.output.1 as i32);
        let mut usable = (0, 0, ow, oh); // x, y, w, h
        for i in 0..self.layers.len() {
            let l = &self.layers[i];
            let (t, r, b, lm) = l.margin;
            let a = l.anchor;
            let both_h = a & (ANCHOR_LEFT | ANCHOR_RIGHT) == (ANCHOR_LEFT | ANCHOR_RIGHT);
            let both_v = a & (ANCHOR_TOP | ANCHOR_BOTTOM) == (ANCHOR_TOP | ANCHOR_BOTTOM);
            let w = if l.size.0 == 0 { ow - lm - r } else { l.size.0 as i32 };
            let h = if l.size.1 == 0 { oh - t - b } else { l.size.1 as i32 };
            let x = match (a & ANCHOR_LEFT != 0, a & ANCHOR_RIGHT != 0) {
                (true, false) => lm,
                (false, true) => ow - w - r,
                _ => (ow - w) / 2,
            };
            let y = match (a & ANCHOR_TOP != 0, a & ANCHOR_BOTTOM != 0) {
                (true, false) => t,
                (false, true) => oh - h - b,
                _ => (oh - h) / 2,
            };
            // Reserve space along the one edge it's attached to.
            if l.zone > 0 {
                if a & ANCHOR_TOP != 0 && !both_v {
                    let edge = t + l.zone;
                    if edge > usable.1 {
                        usable.3 -= edge - usable.1;
                        usable.1 = edge;
                    }
                } else if a & ANCHOR_BOTTOM != 0 && !both_v {
                    usable.3 = usable.3.min(oh - b - l.zone - usable.1);
                } else if a & ANCHOR_LEFT != 0 && !both_h {
                    let edge = lm + l.zone;
                    if edge > usable.0 {
                        usable.2 -= edge - usable.0;
                        usable.0 = edge;
                    }
                } else if a & ANCHOR_RIGHT != 0 && !both_h {
                    usable.2 = usable.2.min(ow - r - l.zone - usable.0);
                }
            }
            let rect = (x, y, w.max(0), h.max(0));
            let changed = !l.configured || (rect.2, rect.3) != (l.rect.2, l.rect.3);
            self.layers[i].rect = rect;
            if changed {
                self.layers[i].configured = true;
                let serial = self.next_serial();
                self.layers[i].resource.configure(serial, rect.2 as u32, rect.3 as u32);
            }
        }
        self.usable = usable;
        self.dirty = true;
    }

    /// Layer surfaces in one layer, bottom to top.
    pub(super) fn layers_in(&self, layer: u32) -> impl Iterator<Item = &Layer> {
        self.layers.iter().filter(move |l| l.layer == layer)
    }
}

impl GlobalDispatch<ZwlrLayerShellV1, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<ZwlrLayerShellV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for State {
    fn request(state: &mut Self, _: &Client, _: &ZwlrLayerShellV1, request: zwlr_layer_shell_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let zwlr_layer_shell_v1::Request::GetLayerSurface { id, surface, layer, .. } = request {
            let resource = init.init(id, LayerData { surface: surface.clone() });
            let layer = match layer {
                WEnum::Value(l) => l as u32,
                _ => TOP,
            };
            state.layers.push(Layer {
                surface,
                resource,
                layer,
                anchor: 0,
                size: (0, 0),
                zone: 0,
                margin: (0, 0, 0, 0),
                configured: false,
                rect: (0, 0, 0, 0),
            });
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, LayerData> for State {
    fn request(state: &mut Self, _: &Client, _: &ZwlrLayerSurfaceV1, request: zwlr_layer_surface_v1::Request, data: &LayerData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let Some(l) = state.layers.iter_mut().find(|l| l.surface == data.surface) else { return };
        // Applied on the next commit (see State::layer_commit).
        match request {
            zwlr_layer_surface_v1::Request::SetSize { width, height } => l.size = (width, height),
            zwlr_layer_surface_v1::Request::SetAnchor { anchor: WEnum::Value(a) } => l.anchor = a.bits(),
            zwlr_layer_surface_v1::Request::SetExclusiveZone { zone } => l.zone = zone,
            zwlr_layer_surface_v1::Request::SetMargin { top, right, bottom, left } => l.margin = (top, right, bottom, left),
            zwlr_layer_surface_v1::Request::SetLayer { layer: WEnum::Value(v) } => l.layer = v as u32,
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, _: &ZwlrLayerSurfaceV1, data: &LayerData) {
        state.layers.retain(|l| l.surface != data.surface);
        state.forget_surface(&data.surface);
        state.arrange_layers();
    }
}

impl State {
    /// A layer surface committed: (re)arrange so its new size, anchor or
    /// zone take effect, and configure it if needed.
    pub(super) fn layer_commit(&mut self, surface: &WlSurface) {
        if self.layers.iter().any(|l| l.surface == *surface) {
            self.arrange_layers();
        }
    }
}
