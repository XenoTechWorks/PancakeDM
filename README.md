# PancakeDM

A desktop environment for Linux. It looks and feels like GNOME, in the
spirit of Budgie, but it's built around what KDE gets right:

- **Configuration**: you can change every part of it.
- **Customizability**: themes, layouts, panels and behavior are yours to shape.
- **Usability**: it works well from the first login and is easy to learn.

The goal is a desktop environment built on those three points that also
looks amazing.

## Why "Pancake"?

The name is a joke on [Sugar](https://sugarlabs.org/) and Sugar on a
Stick, the desktop built to help children in Africa and around the world
learn how to use computers. Sugar goes on pancakes.

## Supported systems

PancakeDM aims to run on a wide range of Linux distributions, from
minimal, source-based systems like KISS Linux to beginner-friendly
distros like Linux Mint.

## Status

Early planning. Nothing to install yet.

## Building

Run `make`, then `make install`. See [docs/architecture.md](docs/architecture.md)
for details. Most code is
Rust (one Cargo workspace), the low-level layer is Zig.
