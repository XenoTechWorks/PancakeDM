//! Animations: windows fade and slide in when they open and fade out when
//! they close (from a snapshot of their last frame), tiles glide to new
//! spots, workspaces slide sideways, and overlays fade in and out. All of
//! it eases out (fast start, gentle stop).

use super::{surface_size, State, SurfaceState};
use crate::render::Canvas;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::Resource;

const OPEN: Duration = Duration::from_millis(220);
const CLOSE: Duration = Duration::from_millis(180);
const GLIDE: Duration = Duration::from_millis(240);
const WORKSPACE: Duration = Duration::from_millis(280);
const LAYER: Duration = Duration::from_millis(170);

/// Ease out (cubic): fast at first, settling gently.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn progress(start: Instant, len: Duration) -> f32 {
    start.elapsed().as_secs_f32() / len.as_secs_f32()
}

/// Where a gliding tile is right now, between where it was and `target`.
pub fn glide(from: Option<((i32, i32), Instant)>, target: (i32, i32)) -> (i32, i32) {
    match from {
        Some(((fx, fy), start)) if start.elapsed() < GLIDE => {
            let p = ease_out(progress(start, GLIDE));
            (fx + ((target.0 - fx) as f32 * p) as i32, fy + ((target.1 - fy) as f32 * p) as i32)
        }
        _ => target,
    }
}

/// A closed window or overlay fading away.
pub struct Ghost {
    px: Vec<u32>,
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    opaque: bool,
    clip: Option<(i32, i32, i32, i32, i32)>,
    start: Instant,
    /// Windows sink a little as they fade; overlays just fade.
    sink: bool,
}

fn snapshot(surface: &WlSurface) -> Option<(Vec<u32>, u32, u32, bool)> {
    let s = surface.data::<Mutex<SurfaceState>>()?.lock().ok()?;
    (s.width > 0).then(|| (s.pixels.clone(), s.width, s.height, s.opaque))
}

impl State {
    /// Anything moving? Then the compositor keeps drawing frames.
    pub fn animating(&self) -> bool {
        self.ws_anim.is_some_and(|(_, _, t)| t.elapsed() < WORKSPACE)
            || !self.ghosts.is_empty()
            || self.windows.iter().any(|w| {
                w.opened_at.is_some_and(|t| t.elapsed() < OPEN) || w.slide_from.is_some_and(|(_, t)| t.elapsed() < GLIDE)
            })
            || self.layers.iter().any(|l| l.mapped_at.is_some_and(|t| t.elapsed() < LAYER))
    }

    /// The workspace switch in progress: (old workspace, its x offset,
    /// the new workspace's x offset).
    pub(super) fn ws_slide(&self) -> Option<(u32, i32, i32)> {
        let (from, dir, start) = self.ws_anim?;
        if start.elapsed() >= WORKSPACE {
            return None;
        }
        let p = ease_out(progress(start, WORKSPACE));
        let w = self.output.0 as f32;
        Some((from, (-(dir as f32) * p * w) as i32, (dir as f32 * (1.0 - p) * w) as i32))
    }

    /// Fade-in amount and upward slide for a window that just opened.
    fn open_state(&self, i: usize) -> (f32, i32) {
        match self.windows[i].opened_at {
            Some(t) if t.elapsed() < OPEN => {
                let p = ease_out(progress(t, OPEN));
                (p, ((1.0 - p) * 18.0 * self.scale as f32) as i32)
            }
            _ => (1.0, 0),
        }
    }

    /// Draw windows (indices bottom to top), shifted by `dx`.
    pub(super) fn draw_windows(&self, canvas: &mut Canvas, order: &[usize], dx: i32) {
        let focused = self.kb_focus.as_ref().and_then(|s| self.root_window(s));
        for &i in order {
            let (x, y) = self.origin(i);
            let w = &self.windows[i];
            let (alpha, dy) = self.open_state(i);
            if w.popup.is_some() {
                self.draw_tree_a(canvas, &w.surface, x + dx, y + dy, None, alpha, 0);
                continue;
            }
            // Toplevels: rounded corners (clipping the app's content) and a
            // border, accent-colored when focused. Maximized: square, none.
            let f = self.frame_rect(i);
            let frame = (f.0 + dx, f.1 + dy, f.2, f.3);
            let (radius, border) = if w.maximized { (0, 0) } else { (self.style.radius, self.style.border) };
            let clip = (radius > 0).then_some((frame.0, frame.1, frame.2, frame.3, radius));
            self.draw_tree_a(canvas, &w.surface, x + dx, y + dy, clip, alpha, 0);
            if border > 0 && frame.2 > 0 {
                let base = if Some(i) == focused { 0xff00_0000 | self.style.accent } else { 0x40ff_ffff };
                let a = ((base >> 24) as f32 * alpha) as u32;
                let color = (a << 24) | (base & 0xff_ffff);
                canvas.ring(frame.0 - border, frame.1 - border, frame.2 + 2 * border, frame.3 + 2 * border, radius + border, border, color);
            }
        }
    }

    /// Draw (and expire) fading ghosts of closed windows and overlays.
    pub(super) fn draw_ghosts(&mut self, canvas: &mut Canvas) {
        self.ghosts.retain(|g| g.start.elapsed() < CLOSE);
        for g in &self.ghosts {
            let p = ease_out(progress(g.start, CLOSE));
            let dy = if g.sink { (p * 14.0 * self.scale as f32) as i32 } else { 0 };
            let clip = g.clip.map(|(cx, cy, cw, ch, r)| (cx, cy + dy, cw, ch, r));
            canvas.blit_ex(g.x, g.y + dy, g.w, g.h, &g.px, g.opaque, clip, 1.0 - p);
        }
    }

    /// Remember window `i`'s last frame so it can fade out after closing.
    pub(super) fn ghost_window(&mut self, i: usize) {
        if !self.windows[i].placed || self.workspace_of(i) != self.active_ws {
            return;
        }
        let Some((px, w, h, opaque)) = snapshot(&self.windows[i].surface) else { return };
        let (x, y) = self.origin(i);
        let f = self.frame_rect(i);
        let r = if self.windows[i].maximized { 0 } else { self.style.radius };
        let clip = (r > 0).then_some((f.0, f.1, f.2, f.3, r));
        self.ghosts.push(Ghost { px, w, h, x, y, opaque, clip, start: Instant::now(), sink: true });
        self.dirty = true;
    }

    /// Same for an overlay (e.g. the launcher closing).
    pub(super) fn ghost_layer(&mut self, surface: &WlSurface) {
        let Some(l) = self.layers.iter().find(|l| l.surface == *surface) else { return };
        if !surface.is_alive() || surface_size(surface).0 == 0 {
            return;
        }
        let Some((px, w, h, opaque)) = snapshot(surface) else { return };
        let (x, y) = (l.rect.0, l.rect.1);
        self.ghosts.push(Ghost { px, w, h, x, y, opaque, clip: None, start: Instant::now(), sink: false });
        self.dirty = true;
    }

    /// Fade-in for a layer surface that just appeared.
    pub(super) fn layer_alpha(&self, l: &super::layer::Layer) -> f32 {
        match l.mapped_at {
            Some(t) if t.elapsed() < LAYER => ease_out(progress(t, LAYER)),
            _ => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_runs_from_zero_to_one() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.5) > 0.5); // eases out: past halfway at half time
    }

    #[test]
    fn glide_ends_at_target() {
        assert_eq!(glide(None, (10, 20)), (10, 20));
        let long_ago = Instant::now() - Duration::from_secs(1);
        assert_eq!(glide(Some(((0, 0), long_ago)), (10, 20)), (10, 20));
        let mid = glide(Some(((0, 0), Instant::now())), (100, 0));
        assert!(mid.0 < 100);
    }
}
