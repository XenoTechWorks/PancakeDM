//! PancakeDM compositor. Window management and policy are written in Rust;
//! the low-level layer (DRM/KMS, input, buffers, seat) is Zig, under `lowlevel/`.

mod backend;
mod cursor;
mod ipc;
mod low;
mod render;
mod wayland;

use backend::{Backend, Event};
use cursor::Pointer;
use low::{key, InputEvent};
use render::Scene;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: pancake-compositor [options]
       pancake-compositor msg COMMAND...

With no options, runs the compositor: apps connect over Wayland
(WAYLAND_DISPLAY is printed at start). Inside another Wayland desktop it
opens as a window (nested); from a TTY it takes the real screens.
Click a window to focus it, Alt+drag moves it. Ctrl+Alt+Backspace quits,
Ctrl+Alt+F1..F12 switches VT.

  --drm              use the real screens even inside a Wayland desktop
  --fill RRGGBB      background color (default f4a259)
  --anim             slide a bar across the background (shows any tearing)
  --seconds N        quit after N seconds (default: never)
  --list             list connected screens and exit
  --input            print keyboard/mouse events and exit on Esc
  --device PATH      DRM device (default /dev/dri/card0)
  --version          print versions and exit

msg commands (to a running compositor): screenshot PATH, windows, quit";

/// Default background: pancake gold.
const DEFAULT_BACKGROUND: u32 = 0xf4a259;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("msg") {
        let cmd: Vec<String> = args.skip(1).collect();
        return match pancake_ipc::send(&cmd.join(" ")) {
            Ok(reply) => {
                println!("{reply}");
                if reply.starts_with("ok") { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => fail(&e),
        };
    }
    let (mut list, mut input, mut fill, mut seconds, mut device) = (false, false, None, 0u64, None);
    let (mut anim, mut force_drm) = (false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--list" => list = true,
            "--input" => input = true,
            "--anim" => anim = true,
            "--drm" => force_drm = true,
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
        let nested = !force_drm && std::env::var_os("WAYLAND_DISPLAY").is_some();
        let backend: Result<Box<dyn Backend>, String> = if nested {
            eprintln!("pancake-compositor: running nested in the current Wayland desktop");
            backend::nested::NestedBackend::new().map(|b| Box::new(b) as Box<dyn Backend>)
        } else {
            backend::drm::DrmBackend::new(device.as_deref()).map(|b| Box::new(b) as Box<dyn Backend>)
        };
        backend.and_then(|mut b| run_session(b.as_mut(), fill.unwrap_or(DEFAULT_BACKGROUND), anim, deadline))
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

/// Ctrl and Alt state, for compositor shortcuts.
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

/// Never start frames closer together than 3/4 of a refresh. Some GPUs
/// (QEMU's virtio-gpu) finish flips instantly instead of at vblank; this
/// keeps us and the apps from spinning at thousands of fps. On real
/// hardware the flip itself is the limit and this never kicks in.
fn min_gap(o: &low::Output) -> Duration {
    let hz = if o.refresh_mhz > 0 { o.refresh_mhz as f64 / 1000.0 } else { 60.0 };
    Duration::from_secs_f64(0.75 / hz)
}

fn run_session(backend: &mut dyn Backend, color: u32, anim: bool, deadline: Option<Instant>) -> Result<(), String> {
    let mut outputs = backend.outputs();
    print_outputs(&outputs);
    let count = outputs.len();

    // Apps get screen 0's size for now.
    let mut server = wayland::Server::new()?;
    let mut wl = wayland::State::new(outputs[0].width, outputs[0].height, outputs[0].refresh_mhz);
    let wl_display_fd = server.display_fd();
    eprintln!("pancake-compositor: WAYLAND_DISPLAY={}", server.name);
    let ipc = ipc::Ipc::bind(&server.name)?;
    eprintln!("pancake-compositor: PANCAKE_SOCKET={}", ipc.path().display());

    // The pointer lives on screen 0 for now; multi-screen layout comes later.
    let mut pointer = Pointer::new(outputs[0].width, outputs[0].height);
    wl.pointer_motion(pointer.x, pointer.y);

    let scene = Scene { color, anim, started: Instant::now() };
    // Per output: needs a new frame / waiting for the last one to land.
    let mut dirty = vec![true; count];
    let mut busy = vec![false; count];
    let mut gaps: Vec<Duration> = outputs.iter().map(min_gap).collect();
    let mut last_frame = vec![Instant::now() - Duration::from_secs(1); count];
    let mut screenshots: Vec<(std::path::PathBuf, std::os::unix::net::UnixStream)> = Vec::new();

    let mut mods = Mods::default();
    let mut active = true;
    let debug_input = std::env::var_os("PANCAKE_DEBUG_INPUT").is_some();
    let mut events = Vec::new();

    while !expired(deadline) {
        // Draw every output that wants a frame and isn't waiting on the last one.
        if active {
            for i in 0..count {
                if !dirty[i] || busy[i] || last_frame[i].elapsed() < gaps[i] {
                    continue;
                }
                last_frame[i] = Instant::now();
                let Some(mut canvas) = backend.begin_frame(i as u32)? else {
                    busy[i] = true;
                    continue;
                };
                scene.draw(&mut canvas);
                wl.draw(&mut canvas);
                if i == 0 && !screenshots.is_empty() {
                    let (w, h, px) = canvas.snapshot();
                    for (path, stream) in screenshots.drain(..) {
                        let msg = match render::write_png(&path, w, h, &px) {
                            Ok(()) => format!("ok {}", path.display()),
                            Err(e) => format!("error {e}"),
                        };
                        ipc::reply(stream, &msg);
                    }
                }
                drop(canvas);
                let shown_now = backend.commit_frame(i as u32)?;
                busy[i] = !shown_now;
                if shown_now {
                    wl.frame_done(scene.started.elapsed().as_millis() as u32);
                }
                // A frame shown right away gets no FrameDone, so when
                // animating, queue the next frame here instead.
                dirty[i] = shown_now && scene.anim;
            }
        }

        // Backend fds first, then: Wayland clients, new clients, control socket.
        let mut fds = backend.fds();
        let nb = fds.len();
        fds.extend([wl_display_fd, server.socket_fd(), ipc.fd()]);
        server.flush();
        // Wake up in time for a frame that's only waiting on the pacing gap.
        let timeout = (0..count)
            .filter(|&i| active && dirty[i] && !busy[i])
            .map(|i| gaps[i].saturating_sub(last_frame[i].elapsed()).as_millis() as i32 + 1)
            .min()
            .unwrap_or(100);
        let ready = low::wait(&fds, timeout);

        if ready[nb + 1] {
            server.accept();
        }
        if ready[nb] {
            server.dispatch(&mut wl);
        }
        if ready[nb + 2] {
            for (cmd, stream) in ipc.accept() {
                match cmd {
                    ipc::Command::Screenshot(path) => {
                        screenshots.push((path, stream));
                        dirty[0] = true;
                    }
                    ipc::Command::Windows => ipc::reply(stream, &format!("ok {}", wl.window_titles().join("|"))),
                    ipc::Command::Quit => {
                        ipc::reply(stream, "ok");
                        return Ok(());
                    }
                }
            }
        }

        backend.dispatch(&ready[..nb], &mut events);
        let before = pointer;
        // Pointer motion is sent to apps once per batch, but always before
        // a click or scroll so they land where the pointer actually is.
        let mut sent = pointer;
        for ev in events.drain(..) {
            match ev {
                Event::FrameDone(mask) => {
                    // Our frame is on screen: apps may draw their next one.
                    wl.frame_done(scene.started.elapsed().as_millis() as u32);
                    for i in (0..count).filter(|i| mask & (1 << i) != 0) {
                        busy[i] = false;
                        // Animating: draw the next frame now that this one is
                        // up, so we render once per refresh.
                        if scene.anim {
                            dirty[i] = true;
                        }
                    }
                }
                Event::Ready(mask) => {
                    for i in (0..count).filter(|i| mask & (1 << i) != 0) {
                        busy[i] = false;
                    }
                }
                Event::Active(on) => {
                    active = on;
                    mods = Mods::default();
                    if on {
                        busy.fill(false);
                        dirty.fill(true);
                    }
                }
                Event::Resized => {
                    outputs = backend.outputs();
                    gaps = outputs.iter().map(min_gap).collect();
                    pointer.resize(outputs[0].width, outputs[0].height);
                    wl.set_output_size(outputs[0].width, outputs[0].height);
                    dirty.fill(true);
                }
                Event::Quit => return Ok(()),
                Event::Input(ev) => {
                    if debug_input {
                        eprintln!("pancake-compositor: input {ev:?} (pointer at {:.0},{:.0})", pointer.x, pointer.y);
                    }
                    match ev {
                        InputEvent::Key { code, pressed } => {
                            mods.update(code, pressed);
                            // Compositor shortcuts first; everything else goes to the app.
                            if pressed && mods.ctrl_alt() {
                                if let Some(n) = key::function_number(code) {
                                    backend.switch_vt(n);
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
            }
        }
        if pointer != sent {
            wl.pointer_motion(pointer.x, pointer.y);
        }
        // One move per batch of events, and only when it actually changed.
        if pointer.pixel() != before.pixel() {
            let (x, y) = pointer.pixel();
            backend.pointer_moved(x, y);
        }
        // An app committed new content (or a window moved or went away).
        if wl.dirty {
            wl.dirty = false;
            dirty.fill(true);
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
