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
Libraries (with headers): libdrm, libinput, libudev. On KISS or other
systems without systemd, eudev or libudev-zero provide libudev.

For now the compositor opens input devices directly, so your user needs
to be in the `input` group (and in `video` for the GPU). Seat handling
through logind or seatd will replace that.

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
