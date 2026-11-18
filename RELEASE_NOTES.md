# PancakeDM 0.1.0: first release

A from-scratch Linux desktop environment: Wayland compositor, panel,
launcher, settings, login screen and tools, in Rust with a little Zig.
Not based on GNOME, KDE or Budgie. Works with any init system.

**This is an early release.** It runs real GTK 4 / libadwaita apps
(tested with Nautilus and pavucontrol), but has only been tested on one
machine, and the real-screen (TTY) path has had far less testing than
running nested inside another Wayland desktop. Rendering is software
only, so expect higher CPU use than a GPU-rendered desktop.

## Highlights

- **Compositor**: dwindle tiling, workspaces 1-9, floating and maximized
  windows, rounded corners and borders, animations (open/close fades,
  tile glides, workspace slides), 1x/2x/3x scaling, copy and paste,
  popup menus, subsurfaces, layer shell.
- **Keybinds** you can change in `pancake.conf` (Super+Return, Super+Q,
  Super+1..9, Super+arrows/HJKL, and more).
- **Panel**: floating "island" bar with a workspace switcher and app
  icons, focused window, now-playing media, clock, CPU/RAM, network,
  Bluetooth, volume and battery, plus a control center (Wi-Fi and
  Bluetooth toggles, volume and brightness sliders, power buttons).
  It ducks away while the display scale or size changes.
- **Dashboard** (click the clock): weather, clock, calendar with your
  events, system info with your distro's logo, gauges and a media player.
- **Low-power mode** (Auto/On/Off in Settings): no animations and slower
  updates, switching on by itself when the CPU passes 90°C.
- **Launcher** with real app icons, **Desktop Settings** with live
  changes, and an accent color that can follow the wallpaper.
- **Login screen**: a greeter for greetd.
- **Tools**: `pancake-fetch` (system info) and `pancake-event`
  (calendar events).

## Install and try

See the [README](README.md). Quickest and safest: run
`./target/release/pancake-compositor` inside your current Wayland
desktop; it opens as a window.

## Known limitations

- Software rendering only (no GPU rendering yet); animations and the
  dashboard cost CPU. On a hot or passively cooled CPU, build with
  `make JOBS=1`.
- Single screen: the cursor and layout use the first output only.
- No lock screen, notifications, screenshots tool or drag and drop yet.
- Calendar events are added with `pancake-event` or by editing a text
  file; there's no click-to-add UI and no reminders yet.
- Fractional scaling (125%, 150%) isn't supported.
- The login screen has only been tested in demo mode, not under a real
  greetd.
- No default wallpaper is shipped; add images to
  `~/.local/share/pancake/wallpapers`.

## Privacy

The dashboard's weather uses wttr.in, which sees your IP address. Set
`weather-location` to a city or `weather = off` in `[panel]` to change
that. Nothing else is sent anywhere.

## Under the hood

Rust workspace (compositor, panel, launcher, settings, greeter, fetch,
config, IPC, shared drawing) plus a Zig library for DRM/KMS, libinput,
seats and the cursor. Built with a plain POSIX `Makefile`. Licensed
GPL-3.0-or-later. Design inspiration from DankMaterialShell and Caelestia
(ideas only, no code).
