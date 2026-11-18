//! Seat access through libseat, which talks to systemd-logind, elogind or
//! seatd: whatever the system runs. The seat hands us GPU and input devices
//! without needing the `video`/`input` groups, and handles VT switching.
//!
//! When the user switches VT away, the seat is "disabled": we must stop
//! touching devices and acknowledge. When they come back it's "enabled".

const std = @import("std");
const builtin = @import("builtin");
const c = @cImport({
    @cInclude("libseat.h");
    @cInclude("unistd.h");
});

const cc: std.builtin.CallingConvention =
    if (builtin.zig_version.order(.{ .major = 0, .minor = 14, .patch = 0 }) == .lt) .C else .c;

const max_devices = 64;

/// Bits returned by pancake_seat_dispatch.
const event_enabled: c_int = 1;
const event_disabled: c_int = 2;

pub const Seat = struct {
    seat: *c.struct_libseat,
    active: bool = false,
    pending: c_int = 0,
    fds: [max_devices]c_int = [_]c_int{-1} ** max_devices,
    ids: [max_devices]c_int = [_]c_int{-1} ** max_devices,

    /// Open a device through the seat. Returns the fd, or -1.
    pub fn openDevice(self: *Seat, path: [*c]const u8) c_int {
        var fd: c_int = -1;
        const id = c.libseat_open_device(self.seat, path, &fd);
        if (id < 0 or fd < 0) return -1;
        for (&self.fds, &self.ids) |*f, *i| {
            if (f.* == -1) {
                f.* = fd;
                i.* = id;
                return fd;
            }
        }
        _ = c.libseat_close_device(self.seat, id);
        _ = c.close(fd);
        return -1;
    }

    pub fn name(self: *Seat) [*c]const u8 {
        return c.libseat_seat_name(self.seat);
    }

    pub fn closeDevice(self: *Seat, fd: c_int) void {
        for (&self.fds, &self.ids) |*f, *i| {
            if (f.* == fd) {
                _ = c.libseat_close_device(self.seat, i.*);
                _ = c.close(fd);
                f.* = -1;
                i.* = -1;
                return;
            }
        }
        _ = c.close(fd);
    }
};

fn onEnable(_: ?*c.struct_libseat, data: ?*anyopaque) callconv(cc) void {
    const s: *Seat = @ptrCast(@alignCast(data.?));
    s.active = true;
    s.pending |= event_enabled;
}

fn onDisable(_: ?*c.struct_libseat, data: ?*anyopaque) callconv(cc) void {
    const s: *Seat = @ptrCast(@alignCast(data.?));
    s.active = false;
    s.pending |= event_disabled;
}

var listener = c.struct_libseat_seat_listener{
    .enable_seat = onEnable,
    .disable_seat = onDisable,
};

pub fn of(p: ?*anyopaque) *Seat {
    return @ptrCast(@alignCast(p.?));
}

/// Open the seat and wait (up to ~5 s) until it's ours. NULL if no seat
/// backend is available; callers can fall back to opening devices directly.
export fn pancake_seat_open() ?*anyopaque {
    const s = std.heap.c_allocator.create(Seat) catch return null;
    s.* = .{ .seat = undefined };
    s.seat = c.libseat_open_seat(&listener, s) orelse {
        std.heap.c_allocator.destroy(s);
        return null;
    };
    var tries: usize = 0;
    while (!s.active and tries < 50) : (tries += 1) {
        if (c.libseat_dispatch(s.seat, 100) < 0) break;
    }
    if (!s.active) {
        _ = c.libseat_close_seat(s.seat);
        std.heap.c_allocator.destroy(s);
        return null;
    }
    s.pending = 0;
    return s;
}

export fn pancake_seat_name(handle: ?*anyopaque) [*c]const u8 {
    return c.libseat_seat_name(of(handle).seat);
}

export fn pancake_seat_fd(handle: ?*anyopaque) c_int {
    return c.libseat_get_fd(of(handle).seat);
}

/// Handle pending seat messages. Returns a bitmask: 1 = enabled (VT switched
/// back to us: redraw everything), 2 = disabled (stop using devices, then
/// call pancake_seat_ack_disable). Negative on error.
export fn pancake_seat_dispatch(handle: ?*anyopaque) c_int {
    const s = of(handle);
    if (c.libseat_dispatch(s.seat, 0) < 0) return -1;
    const ev = s.pending;
    s.pending = 0;
    return ev;
}

/// Tell the seat we've stopped using our devices after a disable.
export fn pancake_seat_ack_disable(handle: ?*anyopaque) void {
    _ = c.libseat_disable_seat(of(handle).seat);
}

export fn pancake_seat_active(handle: ?*anyopaque) c_int {
    return @intFromBool(of(handle).active);
}

/// Switch to another VT/session (Ctrl+Alt+F<n>).
export fn pancake_seat_switch(handle: ?*anyopaque, session: c_int) c_int {
    return c.libseat_switch_session(of(handle).seat, session);
}

/// Open a device (e.g. /dev/dri/card0) through the seat. fd or -1.
export fn pancake_seat_open_device(handle: ?*anyopaque, path: [*:0]const u8) c_int {
    return of(handle).openDevice(path);
}

export fn pancake_seat_close_device(handle: ?*anyopaque, fd: c_int) void {
    of(handle).closeDevice(fd);
}

/// Close every device still open and give the seat back.
export fn pancake_seat_close(handle: ?*anyopaque) void {
    if (handle == null) return;
    const s = of(handle);
    for (s.fds) |fd| if (fd != -1) s.closeDevice(fd);
    _ = c.libseat_close_seat(s.seat);
    std.heap.c_allocator.destroy(s);
}
