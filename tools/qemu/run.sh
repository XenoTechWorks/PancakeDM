#!/bin/sh
# Boot PancakeDM in QEMU from a tiny initramfs built out of the host's own
# kernel, udev and libraries, so there's nothing to download.
#
#   tools/qemu/run.sh              build image + boot with a window
#   tools/qemu/run.sh --headless   no window (screenshots via the monitor)
#   PANCAKE_ARGS="--fill 5b8e7d" tools/qemu/run.sh   change compositor options
#   PANCAKE_DEBUG_INPUT=1 tools/qemu/run.sh          log every input event
#
# Screenshots, mouse and keys: tools/qemu/vmctl.py (see its --help).
# Run `make` first. Only one VM at a time.

set -eu
cd "$(dirname "$0")/../.."
ROOT=$PWD
KVER=${KVER:-$(uname -r)}
KERNEL=${KERNEL:-/boot/vmlinuz-$KVER}
OUT=$ROOT/out/qemu
STAGE=$OUT/root
BIN=$ROOT/target/release/pancake-compositor
DISPLAY_OPT="-display gtk,zoom-to-fit=on"
PANCAKE_ARGS=${PANCAKE_ARGS:-}
HELLO=$ROOT/target/release/pancake-hello
[ "${1:-}" = --headless ] && DISPLAY_OPT="-display none"

[ -x "$BIN" ] || { echo "run 'make' first" >&2; exit 1; }
[ -r "$KERNEL" ] || { echo "can't read $KERNEL (set KERNEL=...)" >&2; exit 1; }
if [ -S "$OUT/monitor.sock" ] && pgrep -f "qemu-system.*$OUT" >/dev/null; then
    echo "a PancakeDM VM is already running" >&2; exit 1
fi

rm -rf "$STAGE"
mkdir -p "$STAGE"/dev "$STAGE"/proc "$STAGE"/sys "$STAGE"/run "$STAGE"/tmp "$STAGE"/etc \
    "$STAGE"/usr/bin "$STAGE"/usr/lib
# Merged /usr layout, so every path a binary or library expects resolves.
ln -s usr/bin "$STAGE/bin"
ln -s usr/bin "$STAGE/sbin"
ln -s usr/lib "$STAGE/lib"
ln -s usr/lib "$STAGE/lib64"
ln -s bin "$STAGE/usr/sbin"
ln -s lib "$STAGE/usr/lib64"

# Copy a file to its real (symlink-resolved) path in the image.
put() {
    real=$(realpath "$1")
    case $real in /bin/*|/sbin/*|/usr/sbin/*) real=/usr/bin/${real##*/} ;; esac
    case $real in /lib/*|/lib64/*|/usr/lib64/*) real=/usr/lib/${real#/*/} ;; esac
    mkdir -p "$STAGE$(dirname "$real")"
    [ -e "$STAGE$real" ] || cp -L "$1" "$STAGE$real"
    # Keep the name we asked for too: libfoo.so.1 -> libfoo.so.1.2.3, sh -> dash.
    name=${1##*/}
    [ "$name" = "${real##*/}" ] || ln -sf "${real##*/}" "$STAGE$(dirname "$real")/$name"
}

# Copy binaries and every library they need.
copy_bin() {
    for f in "$@"; do
        put "$f"
        ldd "$f" 2>/dev/null | awk '/=>/ {print $3} /^\t\// {print $1}' | while read -r lib; do
            [ -e "$lib" ] && put "$lib"
        done
    done
}

copy_bin "$(command -v sh)" "$(command -v mount)" "$(command -v sleep)" \
    "$(command -v kmod)" "$(command -v udevd)" "$(command -v udevadm)" \
    "$(command -v setsid)" "$(command -v cat)" "$(command -v mkdir)" "$BIN" "$HELLO"
ln -sf kmod "$STAGE/usr/bin/modprobe"
cp "$BIN" "$STAGE/usr/bin/pancake-compositor"
cp "$HELLO" "$STAGE/usr/bin/pancake-hello"

# udev rules (input devices need ID_INPUT tags for libinput) and libinput quirks.
for d in /usr/lib/udev /etc/udev /usr/share/libinput /usr/share/X11/xkb; do
    [ -d "$d" ] && { mkdir -p "$STAGE$d"; cp -rL "$d/." "$STAGE$d/"; }
done

# Kernel modules for QEMU's virtual GPU and input, plus their dependencies.
MODS="virtio_pci virtio_gpu virtio_input evdev psmouse"
for m in $MODS; do
    modprobe -S "$KVER" --show-depends "$m" 2>/dev/null | awk '$1=="insmod" {print $2}'
done | sort -u | while read -r ko; do
    put "$ko"
done
mkdir -p "$STAGE/usr/lib/modules/$KVER"
cp /lib/modules/"$KVER"/modules.* "$STAGE/usr/lib/modules/$KVER/" 2>/dev/null || true

cat > "$STAGE/init" <<INIT
#!/bin/sh
export PATH=/usr/bin:/usr/sbin:/bin:/sbin
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev
mount -t tmpfs run /run
mount -t tmpfs tmp /tmp
for m in $MODS; do modprobe \$m; done
udevd --daemon
udevadm trigger --action=add
udevadm settle
export XDG_RUNTIME_DIR=/run/user/0
${PANCAKE_DEBUG_INPUT:+export PANCAKE_DEBUG_INPUT=1}
mkdir -p \$XDG_RUNTIME_DIR
echo "pancake: starting compositor" >/dev/ttyS0
(
    while :; do
        setsid -c sh -c 'pancake-compositor $PANCAKE_ARGS' \
            </dev/tty1 >/dev/tty1 2>/dev/ttyS0
        echo "pancake: compositor exited (\$?), restarting in 2s" >/dev/ttyS0
        sleep 2
    done
) &
# Then a test app inside it.
sleep 3
export WAYLAND_DISPLAY=wayland-1
while :; do
    pancake-hello 2>/dev/ttyS0
    echo "pancake: pancake-hello exited (\$?), restarting in 2s" >/dev/ttyS0
    sleep 2
done
INIT
chmod +x "$STAGE/init"

echo "==> packing initramfs"
(cd "$STAGE" && find . | cpio -o -H newc --quiet | gzip -1) > "$OUT/initramfs.gz"

KVM=
[ -w /dev/kvm ] && KVM="-enable-kvm -cpu host"
echo "==> booting (serial log: out/qemu/serial.log)"
# vmport=off: a plain PS/2 mouse instead of VMware's VMMouse, whose moves
# from the monitor (vmctl.py mouse) don't produce motion events.
exec qemu-system-x86_64 $KVM -machine pc,vmport=off -smp 2 -m 1G \
    -kernel "$KERNEL" -initrd "$OUT/initramfs.gz" \
    -append "rdinit=/init console=ttyS0 console=tty1 quiet loglevel=3" \
    -device virtio-vga \
    -serial file:"$OUT/serial.log" \
    -monitor unix:"$OUT/monitor.sock",server,nowait \
    $DISPLAY_OPT
