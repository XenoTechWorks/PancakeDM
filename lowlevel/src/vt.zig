//! Taking over the virtual terminal: stop the kernel console from drawing
//! text and stop keys from reaching the shell behind us. Always restored,
//! including on crashes and kill signals, so the TTY is never left unusable.

const std = @import("std");
const builtin = @import("builtin");
const c = @cImport({
    @cInclude("sys/ioctl.h");
    @cInclude("linux/kd.h");
    @cInclude("linux/vt.h");
    @cInclude("fcntl.h");
    @cInclude("unistd.h");
    @cInclude("signal.h");
});

const cc: std.builtin.CallingConvention =
    if (builtin.zig_version.order(.{ .major = 0, .minor = 14, .patch = 0 }) == .lt) .C else .c;

var tty_fd: c_int = -1;
var saved_kb_mode: c_int = 0;

const fatal_signals = [_]c_int{ c.SIGTERM, c.SIGINT, c.SIGHUP, c.SIGQUIT, c.SIGSEGV, c.SIGABRT, c.SIGBUS, c.SIGFPE, c.SIGILL };

/// 0 = taken, -1 = can't open /dev/tty, -2 = not on a Linux VT (e.g. inside
/// a desktop terminal; nothing to take, which is fine).
export fn pancake_vt_take() c_int {
    if (tty_fd >= 0) return 0;
    const fd = c.open("/dev/tty", c.O_RDWR | c.O_CLOEXEC);
    if (fd < 0) return -1;
    var st: c.struct_vt_stat = undefined;
    if (c.ioctl(fd, c.VT_GETSTATE, &st) != 0 or c.ioctl(fd, c.KDGKBMODE, &saved_kb_mode) != 0) {
        _ = c.close(fd);
        return -2;
    }
    tty_fd = fd;
    for (fatal_signals) |sig| _ = c.signal(sig, onFatalSignal);
    _ = c.ioctl(fd, c.KDSKBMODE, @as(c_ulong, c.K_OFF));
    _ = c.ioctl(fd, c.KDSETMODE, @as(c_ulong, c.KD_GRAPHICS));
    return 0;
}

export fn pancake_vt_release() void {
    restore();
}

fn restore() void {
    if (tty_fd < 0) return;
    _ = c.ioctl(tty_fd, c.KDSETMODE, @as(c_ulong, c.KD_TEXT));
    _ = c.ioctl(tty_fd, c.KDSKBMODE, @as(c_ulong, @intCast(saved_kb_mode)));
    _ = c.close(tty_fd);
    tty_fd = -1;
}

fn onFatalSignal(sig: c_int) callconv(cc) void {
    restore();
    _ = c.signal(sig, c.SIG_DFL);
    _ = c.raise(sig);
}
