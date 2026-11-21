//! pancake-greeter: the PancakeDM login screen, a greeter for greetd.
//!
//! Runs inside `pancake-compositor --greeter` (greetd starts that), as a
//! full-screen overlay: a big clock and date, your avatar and name, and a
//! password field, in the style of the rest of the shell. Arrows switch
//! users, Enter logs in. Without greetd (no $GREETD_SOCK) it runs in demo
//! mode and accepts any password, for testing.
//!
//! With `--lock` it's the lock screen instead: it only unlocks the user
//! who is running it, checks the password with PAM's `unix_chkpwd`
//! helper, and exits successfully when it's right (the compositor treats
//! that as "unlock" and respawns the lock screen if it ever dies).

mod greetd;

use chrono::Local;
use greetd::{Greetd, Response};
use pancake_draw::color::*;
use pancake_draw::icons::Icons;
use pancake_draw::images::{self, Image};
use pancake_draw::{Canvas, Fonts};
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use xkbcommon::xkb;

const KEY_ESC: u32 = 1;
const KEY_BACKSPACE: u32 = 14;
const KEY_ENTER: u32 = 28;
const KEY_KPENTER: u32 = 96;
const KEY_UP: u32 = 103;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;
const KEY_DOWN: u32 = 108;

/// A person who can log in.
struct User {
    name: String,
    /// Full name from the passwd comment field, if any.
    display: String,
    avatar: Option<Image>,
}

/// Regular users: uid 1000+ with a real login shell.
fn users() -> Vec<User> {
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    passwd
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            let uid: u32 = f.get(2)?.parse().ok()?;
            let shell = f.get(6)?;
            if !(1000..60000).contains(&uid) || shell.ends_with("nologin") || shell.ends_with("false") {
                return None;
            }
            let name = f[0].to_string();
            let display = f.get(4).and_then(|g| g.split(',').next()).filter(|g| !g.is_empty()).unwrap_or(&f[0]).to_string();
            let home = f.get(5).copied().unwrap_or("");
            // ~/.face, then AccountsService's icon.
            let avatar = [format!("{home}/.face"), format!("/var/lib/AccountsService/icons/{name}")].iter().find_map(|p| images::load(std::path::Path::new(p), None));
            Some(User { name, display, avatar })
        })
        .collect()
}

enum Phase {
    Typing,
    Busy,
    Done,
}

struct Greeter {
    configured: bool,
    size: (u32, u32),
    running: bool,
    dirty: bool,
    users: Vec<User>,
    current: usize,
    password: String,
    message: Option<(String, bool)>, // text, is_error
    phase: Phase,
    xkb: Option<xkb::State>,
    keys: Vec<u32>,
    pointer: Option<(f64, f64)>,
    clicked: Option<(f64, f64)>,
    buffer_free: [bool; 2],
    shake: Option<Instant>,
    lock: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("pancake-greeter: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut fonts = Fonts::load()?;
    let cfg = pancake_config::load();
    let mut icons = Icons::new(&cfg.icon_theme);
    let accent = 0xff00_0000 | pancake_draw::accent::effective(&cfg);
    let scale = cfg.scale;
    let lock_mode = std::env::args().any(|a| a == "--lock");
    let mut greetd = if lock_mode { None } else { Greetd::connect() };
    let session_cmd = std::env::var("PANCAKE_GREETER_SESSION").unwrap_or_else(|_| "pancake-session".into());
    let hostname = pancake_fetch::hostname().unwrap_or_default();

    let conn = Connection::connect_to_env().map_err(|e| format!("can't connect to Wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<Greeter>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).map_err(|e| e.to_string())?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| e.to_string())?;
    let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).map_err(|_| "no layer shell".to_string())?;
    let _seat: Option<wl_seat::WlSeat> = globals.bind(&qh, 1..=5, ()).ok();
    let surface = compositor.create_surface(&qh, ());
    let layer = layer_shell.get_layer_surface(&surface, None, zwlr_layer_shell_v1::Layer::Overlay, "greeter".into(), &qh, ());
    use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.set_buffer_scale(scale as i32);
    surface.commit();

    let users = users();
    let mut g = Greeter {
        configured: false,
        size: (0, 0),
        running: true,
        dirty: true,
        current: 0,
        users,
        password: String::new(),
        message: if lock_mode {
            Some(("Locked. Type your password to unlock".to_string(), false))
        } else {
            greetd.is_none().then(|| ("Demo mode: greetd isn't running, any password works".to_string(), false))
        },
        phase: Phase::Typing,
        xkb: None,
        keys: Vec::new(),
        pointer: None,
        clicked: None,
        buffer_free: [true; 2],
        shake: None,
        lock: lock_mode,
    };
    if lock_mode {
        // Only the person logged in can unlock.
        let me = pancake_fetch::user().unwrap_or_default();
        g.users.retain(|u| u.name == me);
    }
    if g.users.is_empty() {
        g.users.push(User {
            name: pancake_fetch::user().unwrap_or_else(|| "user".into()),
            display: "User".into(),
            avatar: None,
        });
    }
    let mut bufs: Vec<(wl_buffer::WlBuffer, memmap2::MmapMut, (u32, u32))> = Vec::new();
    let mut hits: Vec<((f32, f32, f32, f32), &'static str)> = Vec::new();
    let mut last_minute = String::new();
    let mut done_at: Option<Instant> = None;
    let fd = conn.backend().poll_fd().as_raw_fd();

    while g.running {
        // Keys typed since last time.
        for key in std::mem::take(&mut g.keys) {
            if !matches!(g.phase, Phase::Typing) {
                continue;
            }
            match key {
                KEY_ENTER | KEY_KPENTER => {
                    g.phase = Phase::Busy;
                    g.message = Some(("Logging in…".into(), false));
                    g.dirty = true;
                    redraw(&mut g, &mut bufs, &shm, &qh, &surface, &mut fonts, &mut icons, accent, scale, &hostname, &mut hits)?;
                    conn.flush().ok();
                    let user = g.users[g.current].name.clone();
                    let result = if lock_mode { check_password(&user, &g.password) } else { login(&mut greetd, &user, &g.password, &session_cmd) };
                    match result {
                        Ok(()) => {
                            g.phase = Phase::Done;
                            g.message = Some((
                                if lock_mode {
                                    "Unlocked".into()
                                } else if greetd.is_some() {
                                    "Welcome!".into()
                                } else {
                                    format!("Demo: would start {session_cmd} for {user}")
                                },
                                false,
                            ));
                            done_at = Some(Instant::now());
                        }
                        Err(e) => {
                            g.phase = Phase::Typing;
                            g.message = Some((e, true));
                            g.password.clear();
                            g.shake = Some(Instant::now());
                        }
                    }
                }
                KEY_BACKSPACE => {
                    g.password.pop();
                }
                KEY_ESC => g.password.clear(),
                KEY_LEFT | KEY_UP if g.password.is_empty() => g.current = (g.current + g.users.len() - 1) % g.users.len(),
                KEY_RIGHT | KEY_DOWN if g.password.is_empty() => g.current = (g.current + 1) % g.users.len(),
                _ => {
                    let t = g.xkb.as_ref().map(|s| s.key_get_utf8(xkb::Keycode::new(key + 8))).unwrap_or_default();
                    if !t.is_empty() && !t.chars().any(char::is_control) {
                        g.password.push_str(&t);
                    }
                }
            }
            g.dirty = true;
        }
        if let Some((x, y)) = g.clicked.take() {
            let (x, y) = (x as f32, y as f32);
            if let Some((_, what)) = hits.iter().find(|(r, _)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3) {
                match *what {
                    "prev" => g.current = (g.current + g.users.len() - 1) % g.users.len(),
                    "next" => g.current = (g.current + 1) % g.users.len(),
                    "reboot" => run_cmd("loginctl", &["reboot"]),
                    "poweroff" => run_cmd("loginctl", &["poweroff"]),
                    _ => {}
                }
                g.dirty = true;
            }
        }
        if done_at.is_some_and(|t| t.elapsed() > Duration::from_millis(900)) {
            break; // greetd starts the session once we exit
        }
        let minute = Local::now().format("%H:%M").to_string();
        if minute != last_minute {
            last_minute = minute;
            g.dirty = true;
        }
        let shaking = g.shake.is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        if shaking {
            g.dirty = true;
        }
        redraw(&mut g, &mut bufs, &shm, &qh, &surface, &mut fonts, &mut icons, accent, scale, &hostname, &mut hits)?;
        conn.flush().map_err(|e| e.to_string())?;
        if let Some(guard) = queue.prepare_read() {
            if pancake_draw::wait_readable(fd, if shaking { 16 } else { 500 }) {
                // "Would block" just means nothing arrived after all; only
                // real errors end the program.
                if let Err(e) = guard.read() {
                    let would_block = matches!(&e, wayland_client::backend::WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock);
                    if !would_block {
                        return Err(e.to_string());
                    }
                }
            }
        }
        queue.dispatch_pending(&mut g).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Check a password with PAM's unix_chkpwd helper (it reads "password\0"
/// on stdin and exits 0 when right). Only works for the user running it.
fn check_password(user: &str, password: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let helper = ["/usr/bin/unix_chkpwd", "/sbin/unix_chkpwd", "/usr/sbin/unix_chkpwd", "/usr/lib/unix_chkpwd"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .ok_or("Can't check passwords here (no unix_chkpwd)")?;
    let mut child = Command::new(helper).args([user, "nullok"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| format!("can't run {helper}: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let _ = stdin.write_all(password.as_bytes());
    let _ = stdin.write_all(&[0]);
    drop(stdin);
    if child.wait().map_err(|e| e.to_string())?.success() {
        Ok(())
    } else {
        Err("Wrong password".into())
    }
}

fn run_cmd(cmd: &str, args: &[&str]) {
    let _ = std::process::Command::new(cmd).args(args).status();
}

/// Talk greetd through a password login. Demo mode (no greetd) accepts
/// anything that isn't empty.
fn login(greetd: &mut Option<Greetd>, user: &str, password: &str, session: &str) -> Result<(), String> {
    let Some(g) = greetd else {
        return if password.is_empty() { Err("Type a password (any will do in demo mode)".into()) } else { Ok(()) };
    };
    let mut reply = g.request(&greetd::create_session(user))?;
    loop {
        match reply {
            Response::Success => break,
            Response::AuthMessage(kind, msg) => match kind.as_str() {
                "secret" | "visible" => reply = g.request(&greetd::auth_response(password))?,
                // Info/error messages need an empty acknowledgement.
                _ => {
                    if kind == "error" {
                        let _ = g.request(&greetd::cancel_session());
                        return Err(msg.trim().to_string());
                    }
                    reply = g.request(&greetd::auth_response(""))?;
                }
            },
            Response::Error(e) => {
                let _ = g.request(&greetd::cancel_session());
                return Err(if e.to_lowercase().contains("auth") { "Wrong password".into() } else { e });
            }
        }
    }
    let cmd: Vec<&str> = session.split_whitespace().collect();
    match g.request(&greetd::start_session(&cmd))? {
        Response::Success => Ok(()),
        Response::Error(e) => Err(e),
        Response::AuthMessage(_, m) => Err(m),
    }
}

/// A round avatar: the user's picture, or the pancake logo.
fn avatar(user: &User, size: u32) -> Image {
    // No picture: a plain tile (the initial is drawn over it by the caller).
    let src = user.avatar.clone().unwrap_or(Image { w: 1, h: 1, px: vec![0xff3a322c] });
    let px = images::wallpaper(&src, size, size, images::Fit::Fill, 0x2a231e);
    let mut img = Image { w: size, h: size, px };
    let r = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - r).powi(2) + (y as f32 + 0.5 - r).powi(2)).sqrt() - r;
            let cover = (0.5 - d).clamp(0.0, 1.0);
            let p = &mut img.px[(y * size + x) as usize];
            let ch = |s: u32| ((((*p >> s) & 0xff) as f32 * cover) as u32) << s;
            *p = ch(24) | ch(16) | ch(8) | ch(0);
        }
    }
    img
}

#[allow(clippy::too_many_arguments)]
fn redraw(
    g: &mut Greeter,
    bufs: &mut Vec<(wl_buffer::WlBuffer, memmap2::MmapMut, (u32, u32))>,
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<Greeter>,
    surface: &wl_surface::WlSurface,
    fonts: &mut Fonts,
    icons: &mut Icons,
    accent: u32,
    scale: u32,
    hostname: &str,
    hits: &mut Vec<((f32, f32, f32, f32), &'static str)>,
) -> Result<(), String> {
    if !g.configured || !g.dirty {
        return Ok(());
    }
    let (w, h) = g.size;
    let (pw, ph) = (w * scale, h * scale);
    if bufs.first().map_or(true, |b| b.2 != (pw, ph)) {
        for b in bufs.drain(..) {
            b.0.destroy();
        }
        for i in 0..2 {
            let (file, mem) = pancake_draw::shm_file((pw * ph * 4) as u64, &format!("greeter-{i}"))?;
            let pool = shm.create_pool(file.as_fd(), (pw * ph * 4) as i32, qh, ());
            let buffer = pool.create_buffer(0, pw as i32, ph as i32, (pw * 4) as i32, wl_shm::Format::Argb8888, qh, i);
            pool.destroy();
            bufs.push((buffer, mem, (pw, ph)));
        }
        g.buffer_free = [true; 2];
    }
    let Some(i) = (0..2).find(|&i| g.buffer_free[i]) else {
        return Ok(());
    };
    let px = pancake_draw::pixels(&mut bufs[i].1);
    let mut c = Canvas::new(px, w as i32, h as i32, scale as f32);
    hits.clear();
    let (w, h) = (w as f32, h as f32);
    c.px.fill(0);
    c.rounded_rect(0.0, 0.0, w, h, 0.0, 0x720b0908); // keep the wallpaper present

    // Treat identity like the media card treats an album: artwork at left,
    // title and secondary information beside it, and one clear control row.
    let (cw, ch) = (520.0, 210.0);
    let shake = g.shake.filter(|t| t.elapsed() < Duration::from_millis(400)).map(|t| {
        let p = t.elapsed().as_secs_f32() / 0.4;
        (p * 6.0 * std::f32::consts::TAU).sin() * 12.0 * (1.0 - p)
    });
    let cx = 48.0 + shake.unwrap_or(0.0);
    let cy = h - ch - 48.0;
    c.rounded_rect(cx, cy, cw, ch, 18.0, 0xf21c1815);

    let now = Local::now();
    let time = now.format("%H:%M").to_string();
    c.text(fonts, &time, 48.0, 118.0, 58.0, true, TEXT);
    let date = now.format("%A, %-d %B").to_string();
    c.text(fonts, &date, 51.0, 148.0, 15.0, false, TEXT_DIM);

    // Identity row.
    let user = &g.users[g.current];
    let (avx, avy) = (cx + 72.0, cy + 82.0);
    c.circle(avx + 2.0, avy + 5.0, 53.0, 0x40000000);
    c.circle(avx, avy, 52.0, 0x24ffffff);
    let size = (96.0 * scale as f32) as u32;
    let av = avatar(user, size);
    c.image(&av, (avx - 48.0) as i32, (avy - 48.0) as i32);
    if user.avatar.is_none() {
        let letter: String = user.display.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
        let lw = fonts.width(&letter, 36.0, true);
        c.text(fonts, &letter, avx - lw / 2.0, avy + 12.0, 36.0, true, accent);
    }
    let tx = cx + 142.0;
    let name = fonts.fit(&user.display, 20.0, true, cw - 182.0);
    c.text(fonts, &name, tx, cy + 43.0, 20.0, true, TEXT);
    if user.display != user.name {
        c.text(fonts, &user.name, tx, cy + 62.0, 12.0, false, TEXT_FAINT);
    }
    if g.users.len() > 1 {
        for (dx, what, label) in [(0.0, "prev", "‹"), (32.0, "next", "›")] {
            let bx = cx + cw - 66.0 + dx;
            let r = (bx - 14.0, cy + 24.0, 28.0, 28.0);
            let lw = fonts.width(label, 18.0, true);
            c.text(fonts, label, bx - lw / 2.0, cy + 43.0, 18.0, true, TEXT_DIM);
            hits.push((r, what));
        }
    }

    // Password field.
    let (fx, fy, fw, fh) = (tx, cy + 82.0, cw - (tx - cx) - 22.0, 48.0);
    let focused_ring = if g.message.as_ref().is_some_and(|(_, e)| *e) { 0xffbc4b51 } else { accent };
    c.rounded_rect(fx - 2.0, fy - 2.0, fw + 4.0, fh + 4.0, 18.0, focused_ring);
    c.rounded_rect(fx, fy, fw, fh, 16.0, 0xff2a231e);
    if g.password.is_empty() {
        let hint = "Password";
        c.text(fonts, hint, fx + 16.0, fy + 32.0, 15.0, false, TEXT_FAINT);
    } else {
        let n = g.password.chars().count().min(24);
        for k in 0..n {
            c.circle(fx + 26.0 + k as f32 * 15.0, fy + fh / 2.0, 5.0, TEXT);
        }
    }
    if matches!(g.phase, Phase::Typing) {
        let caret_x = fx + 16.0 + g.password.chars().count().min(24) as f32 * 15.0 + if g.password.is_empty() { 0.0 } else { 6.0 };
        c.rounded_rect(caret_x, fy + 14.0, 2.0, 24.0, 1.0, accent);
    }
    let enter = (fx + fw - 44.0, fy + 8.0, 36.0, 36.0);
    c.circle(enter.0 + 18.0, enter.1 + 18.0, 18.0, accent);
    // Theme icons (the font may not have arrow or power symbols).
    let mut sym = |c: &mut Canvas, name: &str, cx: f32, cy: f32, size: f32, color: u32| {
        if let Some(img) = icons.symbolic(name, (size * scale as f32) as u32, color) {
            c.image_alpha(&img, cx - size / 2.0, cy - size / 2.0, 1.0);
        }
    };
    sym(&mut c, "go-next-symbolic", enter.0 + 18.0, enter.1 + 18.0, 18.0, text_on(accent & 0xff_ffff));

    if let Some((msg, err)) = &g.message {
        let msg = fonts.fit(msg, 13.0, false, cw - 60.0);
        c.text(fonts, &msg, tx, cy + 158.0, 13.0, false, if *err { 0xffe8848a } else { TEXT_DIM });
    }
    let hint = if g.lock { "Enter to unlock" } else { "Enter to log in · arrows switch user" };
    let hw = fonts.width(hint, 11.0, false);
    c.text(fonts, hint, cx + cw - 22.0 - hw, cy + ch - 18.0, 11.0, false, TEXT_FAINT);

    // Corner: host name and power buttons.
    c.text(fonts, hostname, 24.0, h - 24.0, 13.0, true, TEXT_DIM);
    let power_buttons: &[(&str, &str)] = if g.lock { &[] } else { &[("poweroff", "system-shutdown-symbolic"), ("reboot", "system-reboot-symbolic")] };
    for (k, (what, icon)) in power_buttons.iter().enumerate() {
        let bx = w - 48.0 - k as f32 * 48.0;
        let r = (bx - 18.0, h - 54.0, 36.0, 36.0);
        let hover = g.pointer.is_some_and(|(x, y)| {
            let (x, y) = (x as f32, y as f32);
            x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
        });
        c.circle(bx, h - 36.0, 18.0, if hover { SUBTLE_HOVER } else { SUBTLE });
        sym(&mut c, icon, bx, h - 36.0, 16.0, TEXT);
        hits.push((r, what));
    }

    surface.attach(Some(&bufs[i].0), 0, 0);
    surface.damage_buffer(0, 0, pw as i32, ph as i32);
    surface.commit();
    g.buffer_free[i] = false;
    g.dirty = false;
    Ok(())
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Greeter {
    fn event(g: &mut Self, layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                g.size = (width.max(1), height.max(1));
                g.configured = true;
                g.dirty = true;
            }
            zwlr_layer_surface_v1::Event::Closed => g.running = false,
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Greeter {
    fn event(g: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                // Safety: the compositor sent a keymap of `size` bytes.
                if let Ok(Some(km)) = unsafe { xkb::Keymap::new_from_fd(&ctx, fd, size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS) } {
                    g.xkb = Some(xkb::State::new(&km));
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(s) = &mut g.xkb {
                    s.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => g.keys.push(key),
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Greeter {
    fn event(g: &mut Self, _: &wl_pointer::WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                g.pointer = Some((surface_x, surface_y));
                g.dirty = true;
            }
            wl_pointer::Event::Button {
                state: WEnum::Value(wl_pointer::ButtonState::Pressed), ..
            } => g.clicked = g.pointer,
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Greeter {
    fn event(_: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for Greeter {
    fn event(g: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            g.buffer_free[*i] = true;
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Greeter {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(Greeter: ignore wl_compositor::WlCompositor);
delegate_noop!(Greeter: ignore wl_surface::WlSurface);
delegate_noop!(Greeter: ignore wl_shm::WlShm);
delegate_noop!(Greeter: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Greeter: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
