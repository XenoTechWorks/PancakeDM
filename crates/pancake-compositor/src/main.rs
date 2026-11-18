//! PancakeDM compositor. Window management and policy are written in Rust;
//! the low-level layer (DRM/KMS, input, buffers, seat) is Zig, under `lowlevel/`.

mod cursor;
mod low;
mod render;
mod wayland;

use cursor::{CursorImage, Pointer};
use low::{key, InputEvent};
use render::Scene;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: pancake-compositor [options]

With no options, runs the compositor: apps connect over Wayland
(WAYLAND_DISPLAY is printed at start). Click a window to focus it,
Alt+drag moves it. Ctrl+Alt+Backspace quits, Ctrl+Alt+F1..F12 switches VT.

  --fill RRGGBB      background color (default f4a259)
  --anim             slide a bar across the background (shows any tearing)
  --seconds N        quit after N seconds (default: never)
  --list             list connected screens and exit
  --input            print keyboard/mouse events and exit on Esc
  --device PATH      DRM device (default /dev/dri/card0)
  --version          print versions and exit";

/// Default background: pancake gold.
const DEFAULT_BACKGROUND: u32 = 0xf4a259;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut list, mut input, mut fill, mut seconds, mut device) = (false, false, None, 0u64, None);
    let mut anim = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--list" => list = true,
            "--input" => input = true,
            "--anim" => anim = true,
            "--fill" => match args.next().and_then(|v| u32::from_str_radix(v.trim_start_matches('#'), 16).ok()) {
                Some(c) => fill = Some(c),
                None => return fail("--fill needs a color like f4a259"),
            },
            "--seconds" => match args.next().and_then(|v| v.parse().ok()) {
                Some(s) => seconds = s,
                None => return fail("--seconds needs a number"),
            },
            "--device" => device = args.next(),
            "--version" => {
                println!("pancake-compositor {}", env!("CARGO_PKG_VERSION"));
                println!("low-level layer: {}", low::version());
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => return fail(&format!("unknown option {other}\n{USAGE}")),
        }
    }
    let deadline = (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds));

    let result = if input {
        run_input(deadline)
    } else if list {
        run_list(device.as_deref())
    } else {
        run_session(device.as_deref(), fill.unwrap_or(DEFAULT_BACKGROUND), anim, deadline)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn print_outputs(outputs: &[low::Output]) {
    for (i, o) in outputs.iter().enumerate() {
        println!(
            "{i}: {} {}x{} @ {}.{:03} Hz",
            o.name, o.width, o.height, o.refresh_mhz / 1000, o.refresh_mhz % 1000
        );
    }
}

fn run_list(device: Option<&str>) -> Result<(), String> {
    let dev = low::Device::open(device)?;
    let outputs = dev.outputs();
    if outputs.is_empty() {
        return Err("no connected screens found".into());
    }
    print_outputs(&outputs);
    Ok(())
}

/// Ctrl and Alt state, for Ctrl+Alt+F<n>.
#[derive(Default)]
struct Mods {
    ctrl_l: bool,
    ctrl_r: bool,
    alt_l: bool,
    alt_r: bool,
}

impl Mods {
    fn update(&mut self, code: u32, pressed: bool) {
        match code {
            key::LEFTCTRL => self.ctrl_l = pressed,
            key::RIGHTCTRL => self.ctrl_r = pressed,
            key::LEFTALT => self.alt_l = pressed,
            key::RIGHTALT => self.alt_r = pressed,
            _ => {}
        }
    }

    fn alt(&self) -> bool {
        self.alt_l || self.alt_r
    }

    fn ctrl_alt(&self) -> bool {
        (self.ctrl_l || self.ctrl_r) && (self.alt_l || self.alt_r)
    }
}

fn run_session(device: Option<&str>, color: u32, anim: bool, deadline: Option<Instant>) -> Result<(), String> {
    // Drop order is the reverse of this: input and screens are released
    // first, then the VT, then the seat.
    let seat = low::Seat::open();
    let _vt = if seat.is_none() { low::Vt::take() } else { None };
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
    print_outputs(&outputs);
    let input = low::Input::open(seat.as_ref())?;
    let count = outputs.len();

    // Apps get screen 0's size for now.
    let mut server = wayland::Server::new()?;
    let mut wl = wayland::State::new(outputs[0].width, outputs[0].height, outputs[0].refresh_mhz);
    let wl_display_fd = server.display_fd();
    eprintln!("pancake-compositor: WAYLAND_DISPLAY={}", server.name);

    // The cursor lives on screen 0 for now; multi-screen layout comes later.
    let main = &outputs[0];
    let img = CursorImage::placeholder_for(main.height, dev.cursor_size());
    let mut pointer = Pointer::new(main.width, main.height);
    // Tell the Wayland side where the pointer starts (no motion needed).
    wl.pointer_motion(pointer.x, pointer.y);
    let setup_cursor = |pointer: &Pointer| -> bool {
        for i in 1..count as u32 {
            dev.hide_cursor(i);
        }
        match dev.set_cursor(0, &img) {
            Ok(()) => {
                let (x, y) = pointer.pixel();
                dev.move_cursor(0, x, y);
                true
            }
            Err(e) => {
                eprintln!("pancake-compositor: {e}; running without a cursor");
                false
            }
        }
    };

    let scene = Scene { color, anim, started: Instant::now() };
    // Per output: needs a new frame / waiting for the GPU to flip the last one.
    let mut dirty = vec![true; count];
    let mut busy = vec![false; count];
    // Some GPUs (QEMU's virtio-gpu) finish flips instantly instead of at
    // vblank. Never start frames closer together than 3/4 of a refresh, so
    // we and the apps don't spin at thousands of fps. On real hardware the
    // flip itself is the limit and this never kicks in.
    let min_gap: Vec<Duration> = outputs
        .iter()
        .map(|o| {
            let hz = if o.refresh_mhz > 0 { o.refresh_mhz as f64 / 1000.0 } else { 60.0 };
            Duration::from_secs_f64(0.75 / hz)
        })
        .collect();
    let mut last_frame = vec![Instant::now() - Duration::from_secs(1); count];
    let mut has_cursor = false;
    let mut first_frame = true;

    let mut mods = Mods::default();
    let mut active = true;
    let debug_input = std::env::var_os("PANCAKE_DEBUG_INPUT").is_some();

    while !expired(deadline) {
        // Draw every output that wants a frame and isn't waiting on a flip.
        if active {
            for i in 0..count {
                if !dirty[i] || busy[i] || last_frame[i].elapsed() < min_gap[i] {
                    continue;
                }
                last_frame[i] = Instant::now();
                let Some(mut canvas) = dev.begin_frame(i as u32)? else {
                    busy[i] = true;
                    continue;
                };
                scene.draw(&mut canvas);
                wl.draw(&mut canvas);
                let shown_now = dev.commit_frame(i as u32)?;
                busy[i] = !shown_now;
                if shown_now {
                    wl.frame_done(scene.started.elapsed().as_millis() as u32);
                }
                // A frame shown by modeset gets no flip event, so when
                // animating, queue the next frame here instead.
                dirty[i] = shown_now && scene.anim;
            }
            if first_frame {
                // After the first modeset, so the cursor sits on our screens.
                has_cursor = setup_cursor(&pointer);
                first_frame = false;
            }
        }

        // Fixed slots: 0 input, 1 GPU, 2 Wayland clients, 3 new clients, 4 seat.
        let mut fds = vec![input.fd(), dev.fd(), wl_display_fd, server.socket_fd()];
        if let Some(s) = &seat {
            fds.push(s.fd());
        }
        server.flush();
        // Wake up in time for a frame that's only waiting on the pacing gap.
        let timeout = (0..count)
            .filter(|&i| active && dirty[i] && !busy[i])
            .map(|i| min_gap[i].saturating_sub(last_frame[i].elapsed()).as_millis() as i32 + 1)
            .min()
            .unwrap_or(100);
        let ready = low::wait(&fds, timeout);

        if ready[3] {
            server.accept();
        }
        if ready[2] {
            server.dispatch(&mut wl);
        }

        if ready[1] {
            let landed = dev.dispatch();
            if landed != 0 {
                // Our frame is on screen: apps may draw their next one.
                wl.frame_done(scene.started.elapsed().as_millis() as u32);
            }
            for i in 0..count {
                if landed & (1 << i) != 0 {
                    busy[i] = false;
                    // Animating: draw the next frame now that this one is up,
                    // so we render exactly once per screen refresh.
                    if scene.anim {
                        dirty[i] = true;
                    }
                }
            }
        }

        if let (Some(s), Some(true)) = (&seat, ready.get(4)) {
            let ev = s.dispatch();
            if ev.disabled {
                // Switched away: let go of everything, then tell the seat.
                active = false;
                input.suspend();
                mods = Mods::default();
                s.ack_disable();
            }
            if ev.enabled {
                active = true;
                input.resume();
                dev.reset();
                busy.fill(false);
                dirty.fill(true);
                first_frame = true;
            }
        }
        // An app committed new content (or a window went away): redraw.
        if wl.dirty {
            wl.dirty = false;
            dirty.fill(true);
        }
        if !active || !ready[0] {
            continue;
        }

        let before = pointer;
        // Pointer motion is sent to apps once per batch, but always before
        // a click or scroll so they land where the pointer actually is.
        let mut sent = pointer;
        while let Some(ev) = input.next() {
            if debug_input {
                eprintln!("pancake-compositor: input {ev:?} (pointer at {:.0},{:.0})", pointer.x, pointer.y);
            }
            match ev {
                InputEvent::Key { code, pressed } => {
                    mods.update(code, pressed);
                    // Compositor shortcuts first; everything else goes to the app.
                    if pressed && mods.ctrl_alt() {
                        if let (Some(n), Some(s)) = (key::function_number(code), &seat) {
                            s.switch_to(n);
                            continue;
                        }
                        if code == key::BACKSPACE {
                            return Ok(());
                        }
                    }
                    wl.key(code, pressed);
                }
                InputEvent::Motion { dx, dy } => pointer.move_by(dx, dy),
                InputEvent::MotionAbsolute { x, y } => pointer.move_to_fraction(x, y),
                InputEvent::Button { code, pressed } => {
                    if pointer != sent {
                        wl.pointer_motion(pointer.x, pointer.y);
                        sent = pointer;
                    }
                    wl.pointer_button(code, pressed, mods.alt());
                }
                InputEvent::Scroll { dx, dy } => {
                    if pointer != sent {
                        wl.pointer_motion(pointer.x, pointer.y);
                        sent = pointer;
                    }
                    wl.pointer_axis(dx, dy);
                }
                _ => {}
            }
        }
        if pointer != sent {
            wl.pointer_motion(pointer.x, pointer.y);
        }
        // One move per batch of events, and only when it actually changed.
        // The cursor plane updates on its own; no new frame needed.
        if has_cursor && pointer.pixel() != before.pixel() {
            let (x, y) = pointer.pixel();
            dev.move_cursor(0, x, y);
        }
    }
    Ok(())
}

fn run_input(deadline: Option<Instant>) -> Result<(), String> {
    let input = low::Input::open(None)?;
    while !expired(deadline) {
        if !input.wait(100) {
            continue;
        }
        while let Some(ev) = input.next() {
            println!("{ev:?}");
            if ev == (InputEvent::Key { code: key::ESC, pressed: true }) {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|d| Instant::now() >= d)
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("pancake-compositor: {msg}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_alt_needs_both() {
        let mut m = Mods::default();
        m.update(key::LEFTCTRL, true);
        assert!(!m.ctrl_alt());
        m.update(key::RIGHTALT, true);
        assert!(m.ctrl_alt());
        m.update(key::LEFTCTRL, false);
        assert!(!m.ctrl_alt());
    }

    #[test]
    fn function_keys_map_to_vt_numbers() {
        assert_eq!(key::function_number(59), Some(1));
        assert_eq!(key::function_number(68), Some(10));
        assert_eq!(key::function_number(87), Some(11));
        assert_eq!(key::function_number(88), Some(12));
        assert_eq!(key::function_number(key::ESC), None);
    }
}
