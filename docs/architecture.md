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
| `crates/pancake-panel` | Rust | Top panel |
| `crates/pancake-settings` | Rust | Settings app |
| `session/` | sh | Session launcher + display-manager entry |

The shell programs run as separate processes, so a crashing panel
doesn't take down the session.

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
