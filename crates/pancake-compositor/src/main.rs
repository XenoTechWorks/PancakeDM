//! PancakeDM compositor. Window management and policy are written in Rust;
//! the low-level layer (DRM/KMS, input, buffers, seat) is Zig, under `lowlevel/`.

mod cursor;
mod low;
mod render;

use cursor::{CursorImage, Pointer};
use low::{key, InputEvent};
use render::Scene;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: pancake-compositor [options]
  --list             list connected screens and exit
  --input            print keyboard/mouse events (Esc quits)
  --fill RRGGBB      fill every screen with a color and show the cursor
                     (run from a TTY); Space cycles colors, Esc quits,
                     Ctrl+Alt+F1..F12 switches VT
  --anim             with --fill: slide a bar across (shows any tearing)
  --seconds N        stop after N seconds (default 60, 0 = no limit)
  --device PATH      DRM device (default /dev/dri/card0)
  --version          print versions and exit";

/// Colors Space cycles through in --fill.
const PALETTE: [u32; 4] = [0xf4a259, 0x8cb369, 0x5b8e7d, 0xbc4b51];

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut list, mut input, mut fill, mut seconds, mut device) = (false, false, None, 60u64, None);
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

    let result = if let Some(color) = fill {
        run_fill(device.as_deref(), color, anim, deadline)
    } else if input {
        run_input(deadline)
    } else if list {
        run_list(device.as_deref())
    } else {
        println!("{USAGE}");
        Ok(())
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

    fn ctrl_alt(&self) -> bool {
        (self.ctrl_l || self.ctrl_r) && (self.alt_l || self.alt_r)
    }
}

fn run_fill(device: Option<&str>, color: u32, anim: bool, deadline: Option<Instant>) -> Result<(), String> {
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

    // The cursor lives on screen 0 for now; multi-screen layout comes later.
    let main = &outputs[0];
    let img = CursorImage::placeholder_for(main.height, dev.cursor_size());
    let mut pointer = Pointer::new(main.width, main.height);
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

    let mut scene = Scene { color, anim, started: Instant::now() };
    // Per output: needs a new frame / waiting for the GPU to flip the last one.
    let mut dirty = vec![true; count];
    let mut busy = vec![false; count];
    let mut has_cursor = false;
    let mut first_frame = true;

    // Start after the current color, so the first Space always changes it.
    let mut next = PALETTE.iter().position(|&c| c == color).map_or(0, |i| (i + 1) % PALETTE.len());
    let mut mods = Mods::default();
    let mut active = true;

    while !expired(deadline) {
        // Draw every output that wants a frame and isn't waiting on a flip.
        if active {
            for i in 0..count {
                if !dirty[i] || busy[i] {
                    continue;
                }
                let Some(mut canvas) = dev.begin_frame(i as u32)? else {
                    busy[i] = true;
                    continue;
                };
                scene.draw(&mut canvas);
                let shown_now = dev.commit_frame(i as u32)?;
                busy[i] = !shown_now;
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

        let mut fds = vec![input.fd(), dev.fd()];
        if let Some(s) = &seat {
            fds.push(s.fd());
        }
        let ready = low::wait(&fds, 100);

        if ready[1] {
            let landed = dev.dispatch();
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

        if let (Some(s), Some(true)) = (&seat, ready.get(2)) {
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
        if !active || !ready[0] {
            continue;
        }

        let before = pointer;
        while let Some(ev) = input.next() {
            match ev {
                InputEvent::Key { code, pressed } => {
                    mods.update(code, pressed);
                    if !pressed {
                        continue;
                    }
                    if let (true, Some(n), Some(s)) = (mods.ctrl_alt(), key::function_number(code), &seat) {
                        s.switch_to(n);
                    } else if code == key::ESC {
                        return Ok(());
                    } else if code == key::SPACE {
                        scene.color = PALETTE[next];
                        next = (next + 1) % PALETTE.len();
                        dirty.fill(true);
                    }
                }
                InputEvent::Motion { dx, dy } => pointer.move_by(dx, dy),
                InputEvent::MotionAbsolute { x, y } => pointer.move_to_fraction(x, y),
                _ => {}
            }
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
