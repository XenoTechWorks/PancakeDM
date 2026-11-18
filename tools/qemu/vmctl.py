#!/usr/bin/env python3
"""Talk to the running PancakeDM VM through QEMU's monitor socket.

  vmctl.py shot [name]        screenshot -> out/screenshots/<date>/<name>.png
  vmctl.py mouse DX DY        move the mouse by DX, DY
  vmctl.py key KEY            press a key (QEMU names: spc, esc, a, alt-a, ...)
  vmctl.py button N           set mouse buttons: 1 = left down, 0 = all up
  vmctl.py quit               power off the VM

Uses only the Python standard library.
"""
import datetime, os, socket, struct, sys, time, zlib

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SOCK = os.path.join(ROOT, "out", "qemu", "monitor.sock")


def monitor(cmd):
    s = socket.socket(socket.AF_UNIX)
    s.connect(SOCK)
    s.settimeout(2)
    time.sleep(0.2)
    s.recv(65536)  # banner
    s.sendall(cmd.encode() + b"\n")
    time.sleep(0.5)
    try:
        s.recv(65536)
    except socket.timeout:
        pass
    s.close()


def ppm_to_png(ppm, png):
    data = open(ppm, "rb").read()
    parts, pos = [], 0
    while len(parts) < 4:  # magic, width, height, maxval (skip comments)
        while data[pos:pos + 1].isspace():
            pos += 1
        if data[pos:pos + 1] == b"#":
            pos = data.index(b"\n", pos)
            continue
        end = pos
        while not data[end:end + 1].isspace():
            end += 1
        parts.append(data[pos:end])
        pos = end
    pos += 1
    w, h = int(parts[1]), int(parts[2])
    px = data[pos:pos + w * h * 3]
    raw = b"".join(b"\0" + px[y * w * 3:(y + 1) * w * 3] for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    with open(png, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 6)))
        f.write(chunk(b"IEND", b""))


def shot(name=None):
    day = datetime.date.today().isoformat()
    d = os.path.join(ROOT, "out", "screenshots", day)
    os.makedirs(d, exist_ok=True)
    name = name or datetime.datetime.now().strftime("%H%M%S")
    ppm, png = os.path.join(d, name + ".ppm"), os.path.join(d, name + ".png")
    monitor("screendump " + ppm)
    for _ in range(20):
        if os.path.exists(ppm) and os.path.getsize(ppm) > 0:
            break
        time.sleep(0.2)
    time.sleep(0.3)
    ppm_to_png(ppm, png)
    os.remove(ppm)
    print(png)


def main(a):
    if not a:
        sys.exit(__doc__)
    if a[0] == "shot":
        shot(a[1] if len(a) > 1 else None)
    elif a[0] == "mouse":
        monitor(f"mouse_move {int(a[1])} {int(a[2])}")
    elif a[0] == "button":
        monitor(f"mouse_button {int(a[1])}")
    elif a[0] == "key":
        monitor("sendkey " + a[1])
    elif a[0] == "quit":
        monitor("quit")
    else:
        sys.exit(__doc__)


main(sys.argv[1:])
