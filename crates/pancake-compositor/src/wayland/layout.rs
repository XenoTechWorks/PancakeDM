//! Workspaces and tiling.
//!
//! Each window lives on a workspace (1-9). Tiled windows on a workspace are
//! laid out "dwindle" style: each one takes half of the space left (split
//! along the longer side), so the newest window gets the last half. Floating
//! windows sit on top where they are; maximized ones fill the free area.

use super::State;
use crate::keybinds::Dir;
use wayland_protocols::xdg::shell::server::xdg_toplevel;
use wayland_server::Resource;

pub type Rect = (i32, i32, i32, i32);

/// Dwindle layout of `n` windows in `area`, with `gap` between them.
pub fn dwindle(area: Rect, n: usize, gap: i32) -> Vec<Rect> {
    let mut out = Vec::with_capacity(n);
    let mut r = area;
    for i in 0..n {
        if i + 1 == n {
            out.push(r);
            break;
        }
        let (x, y, w, h) = r;
        if w >= h {
            let left = (w - gap) / 2;
            out.push((x, y, left, h));
            r = (x + left + gap, y, w - left - gap, h);
        } else {
            let top = (h - gap) / 2;
            out.push((x, y, w, top));
            r = (x, y + top + gap, w, h - top - gap);
        }
    }
    out
}

/// The rect in `dir` from `from` that's closest (straight-line distance in
/// that direction counts most; sideways offset counts double).
pub fn nearest(from: Rect, others: &[Rect], dir: Dir) -> Option<usize> {
    let c = |r: Rect| (r.0 as f64 + r.2 as f64 / 2.0, r.1 as f64 + r.3 as f64 / 2.0);
    let (fx, fy) = c(from);
    others
        .iter()
        .enumerate()
        .filter_map(|(i, &r)| {
            let (x, y) = c(r);
            let (along, side) = match dir {
                Dir::Left => (fx - x, (y - fy).abs()),
                Dir::Right => (x - fx, (y - fy).abs()),
                Dir::Up => (fy - y, (x - fx).abs()),
                Dir::Down => (y - fy, (x - fx).abs()),
            };
            (along > 1.0).then_some((i, along + 2.0 * side))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Visual style and defaults from the config.
#[derive(Clone, Copy)]
pub struct Style {
    pub gaps: i32,
    pub radius: i32,
    pub border: i32,
    /// 0xRRGGBB.
    pub accent: u32,
    pub new_windows_float: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            gaps: 10,
            radius: 12,
            border: 2,
            accent: 0xf4a259,
            new_windows_float: false,
        }
    }
}

impl State {
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
        self.relayout();
    }

    pub fn active_workspace(&self) -> u32 {
        self.active_ws
    }

    /// Workspace a window (or popup, through its parent) is on.
    pub(super) fn workspace_of(&self, i: usize) -> u32 {
        self.root_window(&self.windows[i].surface).map_or(self.windows[i].workspace, |r| self.windows[r].workspace)
    }

    fn is_tiled(&self, i: usize) -> bool {
        let w = &self.windows[i];
        w.toplevel.is_some() && !w.floating && !w.maximized
    }

    /// Recompute where tiled and maximized windows go, and tell any whose
    /// size or state changed.
    pub fn relayout(&mut self) {
        let (ux, uy, uw, uh) = self.usable;
        let g = self.style.gaps;
        // Where each window is now, so moved tiles can glide from there.
        let before: Vec<Option<(i32, i32)>> = (0..self.windows.len()).map(|i| (self.windows[i].placed && self.windows[i].tile.is_some()).then(|| self.origin(i))).collect();
        let old_tiles: Vec<Option<Rect>> = self.windows.iter().map(|w| w.tile).collect();
        for ws in 1..=9 {
            let mut tiled: Vec<usize> = (0..self.windows.len()).filter(|&i| self.windows[i].workspace == ws && self.is_tiled(i)).collect();
            tiled.sort_by_key(|&i| self.windows[i].order);
            let rects = dwindle((ux + g, uy + g, uw - 2 * g, uh - 2 * g), tiled.len(), g);
            for (&i, &r) in tiled.iter().zip(&rects) {
                self.windows[i].tile = Some(r);
            }
        }
        for i in 0..self.windows.len() {
            let w = &mut self.windows[i];
            if w.toplevel.is_none() {
                continue;
            }
            if w.maximized {
                w.tile = Some((ux, uy, uw, uh));
            } else if w.floating {
                w.tile = None;
            }
            if let (Some(from), Some(_)) = (before[i], self.windows[i].tile) {
                if old_tiles[i] != self.windows[i].tile {
                    self.windows[i].slide_from = Some((from, std::time::Instant::now()));
                }
            }
            self.send_configure(i);
        }
        self.dirty = true;
    }

    /// Tell window `i` its size and states, if they changed.
    pub(super) fn send_configure(&mut self, i: usize) {
        let w = &self.windows[i];
        let Some(tl) = w.toplevel.clone() else { return };
        let mut states: Vec<u32> = Vec::new();
        if w.activated {
            states.push(xdg_toplevel::State::Activated as u32);
        }
        if w.maximized {
            states.push(xdg_toplevel::State::Maximized as u32);
        }
        if w.tile.is_some() && !w.maximized && tl.version() >= 2 {
            states.extend([5, 6, 7, 8]); // tiled left/right/top/bottom: no shadows
        }
        let size = match w.tile {
            Some((_, _, tw, th)) => (tw, th),
            None => w.float_size.unwrap_or((0, 0)),
        };
        let bytes: Vec<u8> = states.iter().flat_map(|s| s.to_ne_bytes()).collect();
        if w.last_configure.as_ref() == Some(&(size, bytes.clone())) {
            return;
        }
        self.windows[i].last_configure = Some((size, bytes.clone()));
        // Apps think in logical pixels.
        tl.configure(size.0 / self.scale, size.1 / self.scale, bytes);
        let serial = self.next_serial();
        self.windows[i].xdg.configure(serial);
    }

    /// Windows to draw on the active workspace, bottom to top: tiled,
    /// maximized, floating, then popups.
    pub(super) fn visible_order(&self) -> Vec<usize> {
        self.order_for(self.active_ws, true)
    }

    /// Like visible_order for any workspace (popups only when asked).
    pub(super) fn order_for(&self, ws: u32, with_popups: bool) -> Vec<usize> {
        let on = |i: usize| self.windows[i].placed && self.workspace_of(i) == ws && (with_popups || self.windows[i].popup.is_none());
        let mut tiled: Vec<usize> = (0..self.windows.len()).filter(|&i| on(i) && self.is_tiled(i)).collect();
        tiled.sort_by_key(|&i| self.windows[i].order);
        let maxed = (0..self.windows.len()).filter(|&i| on(i) && self.windows[i].toplevel.is_some() && self.windows[i].maximized && !self.windows[i].floating);
        let floating = (0..self.windows.len()).filter(|&i| on(i) && self.windows[i].toplevel.is_some() && self.windows[i].floating);
        let popups = (0..self.windows.len()).filter(|&i| on(i) && self.windows[i].popup.is_some());
        tiled.into_iter().chain(maxed).chain(floating).chain(popups).collect()
    }

    /// The window's frame on screen (its geometry rect).
    pub(super) fn frame_rect(&self, i: usize) -> Rect {
        let (ox, oy) = self.origin(i);
        match self.windows[i].geometry {
            Some((gx, gy, gw, gh)) => (ox + gx, oy + gy, gw, gh),
            None => {
                let (w, h) = super::surface_size(&self.windows[i].surface);
                (ox, oy, w, h)
            }
        }
    }

    fn focused_toplevel(&self) -> Option<usize> {
        self.kb_focus.as_ref().and_then(|s| self.root_window(s)).filter(|&i| self.windows[i].toplevel.is_some())
    }

    fn focus_index(&mut self, i: usize) {
        let s = self.windows[i].surface.clone();
        if self.windows[i].floating {
            self.raise(i);
        }
        self.set_keyboard_focus(Some(s));
        self.refresh_pointer_focus();
    }

    /// Give the keyboard to the best window on the active workspace: the
    /// one focused there last, else the topmost.
    fn focus_something(&mut self) {
        let ws = self.active_ws;
        let remembered = self.last_focus[ws as usize];
        let target = self
            .windows
            .iter()
            .position(|w| w.id == remembered && w.workspace == ws && w.toplevel.is_some() && w.placed)
            .or_else(|| self.visible_order().into_iter().rev().find(|&i| self.windows[i].toplevel.is_some()));
        match target {
            Some(i) => self.focus_index(i),
            None => {
                self.set_keyboard_focus(None);
                self.refresh_pointer_focus();
            }
        }
    }

    pub(super) fn focus_after_unlock(&mut self) {
        self.focus_something();
    }

    pub fn focus_dir(&mut self, dir: Dir) {
        let Some(cur) = self.focused_toplevel() else {
            return self.focus_something();
        };
        let cands: Vec<usize> = self.visible_order().into_iter().filter(|&i| i != cur && self.windows[i].toplevel.is_some()).collect();
        let rects: Vec<Rect> = cands.iter().map(|&i| self.frame_rect(i)).collect();
        if let Some(k) = nearest(self.frame_rect(cur), &rects, dir) {
            self.focus_index(cands[k]);
        }
    }

    pub fn swap_dir(&mut self, dir: Dir) {
        let Some(cur) = self.focused_toplevel().filter(|&i| self.is_tiled(i)) else {
            return;
        };
        let cands: Vec<usize> = self.visible_order().into_iter().filter(|&i| i != cur && self.is_tiled(i)).collect();
        let rects: Vec<Rect> = cands.iter().map(|&i| self.frame_rect(i)).collect();
        if let Some(k) = nearest(self.frame_rect(cur), &rects, dir) {
            let other = cands[k];
            let (a, b) = (self.windows[cur].order, self.windows[other].order);
            self.windows[cur].order = b;
            self.windows[other].order = a;
            self.relayout();
        }
    }

    pub fn switch_workspace(&mut self, ws: u32) {
        if ws == self.active_ws || !(1..=9).contains(&ws) {
            return;
        }
        self.dismiss_popups();
        let dir = if ws > self.active_ws { 1 } else { -1 };
        self.ws_anim = Some((self.active_ws, dir, std::time::Instant::now()));
        self.active_ws = ws;
        self.dirty = true;
        self.focus_something();
    }

    pub fn move_to_workspace(&mut self, ws: u32) {
        let Some(i) = self.focused_toplevel() else {
            return;
        };
        if ws == self.windows[i].workspace || !(1..=9).contains(&ws) {
            return;
        }
        self.windows[i].workspace = ws;
        self.last_focus[ws as usize] = self.windows[i].id;
        self.relayout();
        self.focus_something();
    }

    pub fn close_focused(&mut self) {
        if let Some(tl) = self.focused_toplevel().and_then(|i| self.windows[i].toplevel.clone()) {
            tl.close();
        }
    }

    /// Float a tiled window (centered, a comfortable size) or tile a
    /// floating one.
    pub fn toggle_floating(&mut self) {
        let Some(i) = self.focused_toplevel() else {
            return;
        };
        // Windows that can't resize (min = max) or dialogs stay floating.
        let w = &self.windows[i];
        if w.floating && ((w.min_size.0 > 0 && w.min_size == w.max_size) || w.has_parent) {
            return;
        }
        let w = &mut self.windows[i];
        w.floating = !w.floating;
        w.maximized = false;
        if w.floating {
            let (ux, uy, uw, uh) = self.usable;
            let (fw, fh) = (uw * 3 / 5, uh * 7 / 10);
            w.float_size = Some((fw, fh));
            w.x = ux + (uw - fw) / 2;
            w.y = uy + (uh - fh) / 2;
        }
        self.relayout();
        if self.windows[i].floating {
            self.raise(i);
        }
    }

    pub fn toggle_maximize(&mut self) {
        let Some(i) = self.focused_toplevel() else {
            return;
        };
        self.windows[i].maximized = !self.windows[i].maximized;
        self.relayout();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwindle_splits_the_longer_side() {
        assert_eq!(dwindle((0, 0, 100, 50), 1, 10), vec![(0, 0, 100, 50)]);
        // Wide: left/right.
        assert_eq!(dwindle((0, 0, 110, 50), 2, 10), vec![(0, 0, 50, 50), (60, 0, 50, 50)]);
        // Then the right half (50x50, square counts as wide) splits again.
        let r = dwindle((0, 0, 110, 50), 3, 10);
        assert_eq!(r[0], (0, 0, 50, 50));
        assert_eq!(r[1], (60, 0, 20, 50));
        assert_eq!(r[2], (90, 0, 20, 50));
        // Tall areas split top/bottom.
        assert_eq!(dwindle((0, 0, 50, 110), 2, 10), vec![(0, 0, 50, 50), (0, 60, 50, 50)]);
        assert!(dwindle((0, 0, 100, 100), 0, 10).is_empty());
    }

    #[test]
    fn nearest_prefers_straight_lines() {
        let me = (100, 100, 100, 100);
        let others = [(300, 100, 100, 100), (100, 300, 100, 100), (300, 400, 100, 100), (-100, 100, 100, 100)];
        assert_eq!(nearest(me, &others, Dir::Right), Some(0));
        assert_eq!(nearest(me, &others, Dir::Down), Some(1));
        assert_eq!(nearest(me, &others, Dir::Left), Some(3));
        assert_eq!(nearest(me, &others, Dir::Up), None);
    }
}
