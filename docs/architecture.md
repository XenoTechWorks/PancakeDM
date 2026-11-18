# Architecture

PancakeDM is its own desktop. It isn't based on GNOME, KDE or Budgie,
although borrowing code from them (and from wlroots, sway, etc.) is fine.

## Languages

- **Rust** for most of the code: the compositor's window management,
  the shell programs, config and IPC. Everything lives in one Cargo workspace.
- **Zig** for the low-level layer: DRM/KMS, libinput, buffers and seat
  access. It's a static library with a C ABI that the Rust compositor links.

## Layout

| Path | Lang | What |
|---|---|---|
| `lowlevel/` | Zig | Low-level layer, `libpancake_low.a` + `pancake_low.h` |
| `crates/pancake-compositor` | Rust | Wayland compositor: windows, workspaces, policy |
| `crates/pancake-config` | Rust | Config loading and live reload |
| `crates/pancake-ipc` | Rust | Compositor ↔ shell IPC (`$XDG_RUNTIME_DIR/pancake.sock`) |
| `crates/pancake-panel` | Rust | Top panel (layer shell, hand-drawn with fontdue) |
| `crates/pancake-settings` | Rust | Settings app |
| `session/` | sh | Session launcher + display-manager entry |

The shell programs run as separate processes, so a crashing panel
doesn't take down the session.

## Frames

Each screen has two buffers. The compositor draws a whole frame into the
hidden one, then asks the GPU to swap them at the next screen refresh
(a page flip). Until the GPU reports the swap through the DRM fd, that
screen is busy and nothing draws to it, so a visible buffer is never
touched and frames can't tear. When animating, the next frame is drawn
as soon as the last one lands: exactly one frame per refresh. Animation
uses elapsed time, not frame counts, so speed is the same at any
refresh rate.

Drawing is in software for now (`render.rs`); GPU rendering can slot in
later behind the same begin-frame / draw / commit flow.

## Init systems

PancakeDM works with systemd, elogind, runit, OpenRC, s6 and anything
else. It never asks users to switch. Seat access goes through logind
when it's available and seatd otherwise.

## Dependencies

Build: Rust (cargo), Zig 0.13 or newer, make.
Libraries (with headers): libdrm, libinput, libudev, libseat. On KISS or
other systems without systemd, eudev or libudev-zero provide libudev.

## Seats

The compositor gets the GPU and input devices through libseat, which
talks to whatever the system runs: systemd-logind, elogind or seatd.
That means no `video`/`input` group membership is needed, and VT
switching (Ctrl+Alt+F1..F12) works: when you switch away the compositor
lets go of its devices, and when you come back it redraws everything.

If no seat backend is running, it falls back to opening devices
directly (then the user needs the `video` and `input` groups) and takes
over the VT itself. VT switching isn't available in that mode.

## Building

PancakeDM uses a plain POSIX `Makefile` that drives Cargo and Zig, so it
works with GNU make, bmake or busybox make.

```sh
make                       # build everything (links Zig if `zig` is installed)
make check                 # cargo check + tests, plus zig build if available
make run                   # run the compositor from the build tree
make PREFIX=/usr DESTDIR="$pkgdir" install   # for packagers
make uninstall
make clean
```

Variables you can override: `PREFIX`, `BINDIR`, `DATADIR`, `DESTDIR`,
`JOBS` (default 2), `PROFILE` (default `release`) and `ZIG_BACKEND`
(`auto`, `yes` or `no`).

## The panel

`pancake-panel` is a separate program the compositor starts at login
(`--no-panel` skips it); it's found next to the compositor binary or in
`PATH`, and gets `WAYLAND_DISPLAY` and `PANCAKE_SOCKET` pointing at the
session. It's a floating rounded bar anchored to the top through
`zwlr_layer_shell_v1`, reserving its space so windows open below it.

It shows the Pancake button, a pill per open window (gold = focused;
click to focus), and the date and time. It draws by hand: rounded
shapes are its own code, text is rasterized with `fontdue` from the font
`fc-match sans-serif` finds. The window list comes from the control
socket (`windows` and `focus ID`, encoded by `pancake-ipc`).

The compositor also supports other layer-shell clients (wallpapers,
docks, launchers) in all four layers.

## Nested mode (development)

Started from inside another Wayland desktop, `pancake-compositor` opens
as a window there and apps can connect to it, with no TTY or VM needed:

```sh
./target/release/pancake-compositor          # prints WAYLAND_DISPLAY=wayland-N
WAYLAND_DISPLAY=wayland-N nautilus           # in another terminal
pancake-compositor msg screenshot /tmp/a.png # PNG of what PancakeDM drew
pancake-compositor msg windows               # open windows (id, focused, title)
pancake-compositor msg focus ID              # raise and focus a window
pancake-compositor msg quit
```

`--drm` forces real screens even inside a desktop. Each compositor has a
control socket at `$XDG_RUNTIME_DIR/pancake-<wayland-N>.sock`;
`msg` finds it through `PANCAKE_SOCKET`, `WAYLAND_DISPLAY`, or by being
the only one running.

## Testing in QEMU

`tools/qemu/run.sh` boots PancakeDM in a VM made from the host's own
kernel, udev and libraries, so nothing gets downloaded. Run `make` first.

```sh
tools/qemu/run.sh               # boot with a window
tools/qemu/run.sh --headless    # no window
tools/qemu/vmctl.py shot NAME   # screenshot -> out/screenshots/<date>/NAME.png
tools/qemu/vmctl.py key spc     # send a key
tools/qemu/vmctl.py mouse 50 0  # move the mouse
tools/qemu/vmctl.py quit        # power off
```

The serial log is in `out/qemu/serial.log`. QEMU screenshots don't include
the hardware cursor (it's on its own plane), so check the cursor in the
window instead.
