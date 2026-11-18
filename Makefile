# PancakeDM build system.
#
# Written in plain POSIX make, so it works with GNU make, bmake and busybox
# make, from KISS Linux to Linux Mint. Packagers can override any variable:
#
#   make PREFIX=/usr JOBS=8
#   make DESTDIR="$pkgdir" PREFIX=/usr install
#
# Zig is optional. If `zig` is found, the low-level layer is built and linked
# into the compositor. Otherwise the compositor builds without it.
# Force a choice with ZIG_BACKEND=yes or ZIG_BACKEND=no.

.POSIX:

PREFIX      = /usr/local
BINDIR      = $(PREFIX)/bin
DATADIR     = $(PREFIX)/share
SESSIONDIR  = $(DATADIR)/wayland-sessions
DOCDIR      = $(DATADIR)/doc/pancakedm
DESTDIR     =

CARGO       = cargo
ZIG         = zig
JOBS        = 2
PROFILE     = release
ZIG_BACKEND = auto

BINS = pancake-compositor pancake-panel pancake-settings

all: build

# ---- build ------------------------------------------------------------------

build:
	@use_zig=$(ZIG_BACKEND); \
	if [ "$$use_zig" = auto ]; then \
		if command -v $(ZIG) >/dev/null 2>&1; then use_zig=yes; else use_zig=no; fi; \
	fi; \
	features=; \
	if [ "$$use_zig" = yes ]; then \
		echo "==> zig: building low-level layer"; \
		(cd lowlevel && $(ZIG) build -Doptimize=ReleaseSafe -j$(JOBS)) || exit 1; \
		features="--features pancake-compositor/zig-backend"; \
	else \
		echo "==> zig: not found, building without the low-level layer"; \
	fi; \
	echo "==> cargo: building ($(PROFILE))"; \
	$(CARGO) build --profile $(PROFILE) -j $(JOBS) $$features

zig:
	cd lowlevel && $(ZIG) build -Doptimize=ReleaseSafe -j$(JOBS)

check:
	$(CARGO) check -j $(JOBS)
	$(CARGO) test -j $(JOBS)
	@if command -v $(ZIG) >/dev/null 2>&1; then cd lowlevel && $(ZIG) build -j$(JOBS); fi

# Run the compositor straight from the build tree.
run: build
	./target/$(PROFILE)/pancake-compositor

# ---- install ----------------------------------------------------------------

install:
	mkdir -p "$(DESTDIR)$(BINDIR)" "$(DESTDIR)$(SESSIONDIR)" "$(DESTDIR)$(DOCDIR)"
	for b in $(BINS); do \
		cp "target/$(PROFILE)/$$b" "$(DESTDIR)$(BINDIR)/$$b" && \
		chmod 755 "$(DESTDIR)$(BINDIR)/$$b" || exit 1; \
	done
	cp session/pancake-session "$(DESTDIR)$(BINDIR)/pancake-session"
	chmod 755 "$(DESTDIR)$(BINDIR)/pancake-session"
	cp session/pancake.desktop "$(DESTDIR)$(SESSIONDIR)/pancake.desktop"
	chmod 644 "$(DESTDIR)$(SESSIONDIR)/pancake.desktop"
	cp README.md docs/architecture.md "$(DESTDIR)$(DOCDIR)/"

uninstall:
	for b in $(BINS) pancake-session; do rm -f "$(DESTDIR)$(BINDIR)/$$b"; done
	rm -f "$(DESTDIR)$(SESSIONDIR)/pancake.desktop"
	rm -rf "$(DESTDIR)$(DOCDIR)"

# ---- clean ------------------------------------------------------------------

clean:
	$(CARGO) clean
	rm -rf lowlevel/zig-out lowlevel/.zig-cache

.PHONY: all build zig check run install uninstall clean
