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

Its launcher button shows the PancakeDM logo (`data/icons/pancakedm.svg`,
built into pancake-draw and installed as the `pancakedm` icon). It's built from "widget islands" (rounded groups, in the spirit of
DankMaterialShell): the Pancake button; a workspace switcher showing each
workspace's app icons, with the active one as an accent pill (click to
switch); the focused window's icon and title; the clock; CPU, memory and
battery (from /proc and /sys); and the settings button. It draws by hand: rounded
shapes are its own code, text is rasterized with `fontdue` from the font
`fc-match sans-serif` finds. The window list comes from the control
socket (`windows` and `focus ID`, encoded by `pancake-ipc`).

The compositor also supports other layer-shell clients (wallpapers,
docks, launchers) in all four layers. Layer surfaces in the top or
overlay layer that ask for exclusive keyboard interactivity get the
keyboard while they're shown.

## The launcher

`pancake-launcher` opens from the panel's Pancake button or a tap of
Super (both go through the compositor's `launcher` command, which opens
it or closes it if it's open). It's a full-screen overlay: the desktop
dims and a card shows a search box over a grid of apps.

Apps come from `.desktop` files in `$XDG_DATA_HOME`, `$XDG_DATA_DIRS`
and the Flatpak export folders; hidden and NoDisplay entries are skipped
and user entries override system ones. Typing searches (names starting
with the text first, then names with a word starting with it, then
anywhere in the name or command). Arrows move, Enter or a click opens,
Esc or a click outside closes. Tiles show a colored letter badge for now;
real icons come later.

Programs the compositor starts get `WAYLAND_DISPLAY`, `PANCAKE_SOCKET`,
`XDG_CURRENT_DESKTOP=Pancake`, and no `DISPLAY`, so apps opened from the
launcher join this session even when nested in another desktop.

The shell programs share `pancake-draw`: the palette, anti-aliased
shapes, text, and small shm/poll helpers.

## Configuration

Everything the user can change lives in one file,
`~/.config/pancake/pancake.conf`, in a simple format of our own:

```ini
[desktop]
wallpaper = pancakes.jpg      # a name in a wallpaper folder, a path, or empty
wallpaper-mode = fill         # fill, fit, center, stretch
background = #f4a259          # shown with no wallpaper, or around "fit"
scale = 1                     # 1, 2 or 3 for HiDPI screens

[panel]
position = top                # top or bottom
clock = 24h                   # 24h or 12h
show-date = true
weather = true                # from wttr.in (sees your IP address)
weather-location =            # a city, or empty to guess from your IP

[theme]
accent = auto                 # from the wallpaper, or a color like #f4a259
icon-theme = Adwaita

[windows]
layout = tiling               # tiling or floating, for new windows
gaps = 10
corner-radius = 12
border = 2

[apps]
terminal = auto               # or a command, e.g. foot

[keybinds]
super+t = spawn foot          # add or override; "none" removes a default
```

Missing keys get defaults, unknown keys are ignored, bad values keep
their default with a warning. The compositor and panel check the file's
modification time about twice a second and apply changes live; Desktop
Settings writes it atomically (temp file, then rename).

Wallpapers are found by name in `~/.local/share/pancake/wallpapers`,
`$XDG_DATA_DIRS/pancake/wallpapers` and `~/Pictures/Wallpapers`. The
compositor decodes the wallpaper once (JPEG or PNG, via the `image`
crate), lays it out for the screen size once, and copies it in each
frame. The default `pancakes.jpg` isn't in the repository (its license
isn't confirmed for redistribution); drop it, or any image, into the
wallpaper folder.

## Windows, tiling and workspaces

Windows live on workspaces 1-9. Tiled windows are laid out "dwindle"
style (each one takes half of the space left, split along the longer
side) inside the area panels leave free, with `gaps` around and between
them. Dialogs and windows that can't resize (min size = max size) float;
so does everything when `layout = floating`. Floating windows sit above
tiled ones; maximized windows fill the free area.

Tiled apps are told they're tiled (xdg_toplevel tiled states), so GTK
drops its drop shadows and rounded corners. The compositor rounds every
window's corners itself (`corner-radius`, clipping the app's content)
and draws a `border`, in the accent color for the focused window.

Alt+drag moves a window; dragging a tiled one floats it at the same size.

### Keybinds

| Keys | Action |
|---|---|
| Super+Return | terminal (`[apps] terminal`, or the first installed TerminalEmulator) |
| Super+Space, or tap Super | launcher |
| Super+Q | close |
| Super+V | toggle floating |
| Super+F | maximize |
| Super+arrows / H J K L | focus left/down/up/right |
| Super+Shift+arrows / H J K L | swap with the window in that direction |
| Super+1..9 | go to workspace |
| Super+Shift+1..9 | move the window to a workspace |
| Super+Shift+E | quit |

Override or add binds in `[keybinds]` (`combo = action`); `none` removes
one. Actions: `terminal`, `launcher`, `close`, `toggle-floating`,
`maximize`, `focus DIR`, `swap DIR`, `workspace N`,
`move-to-workspace N`, `spawn COMMAND`, `quit`. Combos match physical
keys, so they don't move when the keyboard layout changes. Any action can
also be run from outside: `pancake-compositor msg action workspace 3`.

## Scaling

`scale` (1, 2 or 3) is the number of pixels per logical pixel. The
compositor tells apps through `wl_output.scale`, so GTK and others draw
sharp 2x/3x buffers and set their buffer scale. Internally the compositor
works in physical pixels and converts at the protocol boundary: window
geometry, subsurface positions, popup placement, configure sizes, layer
sizes/margins/zones and pointer coordinates. Buffers whose scale doesn't
match the output (older apps) are resized on commit. The config's gaps,
radius and border are logical too.

The shell programs draw through `pancake-draw`'s `Canvas`, which takes a
scale: drawing code uses logical coordinates and gets crisp output.
Fractional scales (125%, 150%) are a later step.

## Animations

Everything eases out (fast start, gentle stop):

- windows fade and slide up a little when they open (220 ms);
- closed windows fade and sink from a snapshot of their last frame
  (180 ms), and so do closing overlays like the launcher;
- tiles glide to their new place after a layout change (240 ms);
- switching workspaces slides the old one out and the new one in (280 ms);
- overlays fade in (170 ms); the panel's cards fade and slide.

While anything moves the compositor draws every frame (paced by the
screen as usual), then goes back to drawing only on changes.

## Media and quick settings

The panel shows what's playing (MPRIS through `playerctl`): album art,
title and artist, and play/pause. Clicking it opens a media card with big
art, a live progress bar and previous/play/next.

Status icons (network, Bluetooth, volume) open the control center: Wi-Fi
and Bluetooth tiles, a volume slider (click the icon to mute) and
brightness when there's a backlight, Desktop Settings, and log out,
suspend, restart and power off (each asks for a second click).

These use common command-line tools: `playerctl`, `wpctl` (PipeWire),
`nmcli` (NetworkManager), `bluetoothctl` (BlueZ), `brightnessctl` and
`loginctl` (logind or elogind). Anything missing is just hidden. They run
on a background thread, publishing each result as it arrives, so the bar
never waits on them. Album art from URLs is downloaded once with `curl`
and cached in `~/.cache/pancake/art`.

`PANCAKE_PANEL_OPEN=control` (or `media`) opens a card at startup, for
testing and screenshots.

## Dashboard

Clicking the clock opens the dashboard (inspired by Caelestia): weather,
a big clock with the date and uptime, system info with the PancakeDM
logo and the palette, this month's calendar, gauges for CPU, temperature,
memory and disk, and media on a record that spins while music plays. The
gauges hide on narrow screens.

Weather comes from wttr.in (`curl`, every 15 minutes). **It sees your IP
address**, which it also uses to guess your location unless you set
`weather-location`. Turn it off with `weather = off`.

## pancake-fetch

`pancake-fetch` prints a fastfetch-style summary next to a pancake stack:
OS, kernel, uptime, packages, shell, desktop, CPU, GPU (via `lspci`),
memory and disk. Package counts come straight from the package databases
of KISS, pacman, dpkg, apk, xbps and rpm, plus Flatpak. The same library
feeds the dashboard.

## Login screen (greetd)

`pancake-greeter` is a greeter for greetd, which works with any init
system. greetd runs `pancake-compositor --greeter`, which starts only the
greeter (no panel, launcher or keybinds) and exits when it does; greetd
then starts the session. The greeter shows a big clock and date, the
user's avatar (`~/.face` or AccountsService, else the logo) and name
(arrows switch between users with uid 1000+ and a login shell), a password
field, and restart/power off. A wrong password shakes the card.

To use it (as root; adjust the greeter user to your distro's):

1. Install greetd and PancakeDM (`make install`).
2. Copy `data/greetd/config.toml` to `/etc/greetd/config.toml`.
3. For the wallpaper on the login screen, copy it to
   `/usr/share/pancake/wallpapers/` (the greeter user can't read your home).
4. Enable greetd with your init system and disable your old display manager.

The session it starts is `pancake-session` (override with
`PANCAKE_GREETER_SESSION`). Without greetd (no `$GREETD_SOCK`) the greeter
runs in demo mode and accepts any password, for testing:
`pancake-compositor --greeter` inside another desktop.

## Accent from the wallpaper

With `accent = auto` (the default), the accent is the wallpaper's most
prominent lively hue (a saturation-weighted hue histogram), tuned to read
well on dark glass, and cached per wallpaper in `~/.cache/pancake`.

## Desktop Settings

`pancake-settings` is a regular window drawn with `pancake-draw`. It
covers the wallpaper (thumbnails of everything in the wallpaper folders,
paged four at a time, plus "No wallpaper") and its mode, the panel's
position, clock style and date, and the accent color. Every click saves
`pancake.conf` immediately. It opens from the panel's settings button
or from the launcher (it's always listed there).

## Icons

`pancake-draw::icons` looks up freedesktop icon themes: the configured
theme, then Adwaita, Breeze, Papirus and hicolor, in the user's,
system and Flatpak icon folders, preferring the exact size, then larger
(downscaled), then smaller, then scalable; then `/usr/share/pixmaps`.
PNGs are scaled with the `image` crate and SVGs rendered at the exact
size with `resvg`. The launcher shows each app's icon; the panel shows
icons in the window pills, matching a window's app id to a desktop entry
(by file id, `StartupWMClass` or program name).

## Nested mode (development)

Started from inside another Wayland desktop, `pancake-compositor` opens
as a window there and apps can connect to it, with no TTY or VM needed:

```sh
./target/release/pancake-compositor          # prints WAYLAND_DISPLAY=wayland-N
WAYLAND_DISPLAY=wayland-N nautilus           # in another terminal
pancake-compositor msg screenshot /tmp/a.png # PNG of what PancakeDM drew
pancake-compositor msg windows               # open windows (id, focused, title)
pancake-compositor msg focus ID              # raise and focus a window
pancake-compositor msg launcher              # open/close the launcher
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
