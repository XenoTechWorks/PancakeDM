//! Keyboard, mouse and touchpad input through libinput + udev.
//! Devices are opened through the seat when there is one; otherwise
//! directly (then the user must be in the `input` group).

const std = @import("std");
const builtin = @import("builtin");
const seat = @import("seat.zig");
const c = @cImport({
    @cInclude("libinput.h");
    @cInclude("libudev.h");
    @cInclude("fcntl.h");
    @cInclude("unistd.h");
});

// Zig 0.13 spells the C calling convention .C; 0.14+ spells it .c.
const cc: std.builtin.CallingConvention =
    if (builtin.zig_version.order(.{ .major = 0, .minor = 14, .patch = 0 }) == .lt) .C else .c;

/// Matches `struct pancake_input_event` in pancake_low.h.
const Event = extern struct {
    kind: u32,
    code: u32,
    pressed: u32,
    _pad: u32 = 0,
    time_usec: u64,
    x: f64,
    y: f64,
    name: [64]u8,
};

const Kind = struct {
    const device_added = 1;
    const device_removed = 2;
    const key = 3;
    const motion = 4;
    const motion_absolute = 5;
    const button = 6;
    const scroll = 7;
};

const Input = struct {
    udev: *c.struct_udev,
    li: *c.struct_libinput,
};

fn openRestricted(path: [*c]const u8, flags: c_int, data: ?*anyopaque) callconv(cc) c_int {
    if (data != null) return seat.of(data).openDevice(path);
    const fd = c.open(path, flags | c.O_CLOEXEC);
    return if (fd < 0) -1 else fd;
}

fn closeRestricted(fd: c_int, data: ?*anyopaque) callconv(cc) void {
    if (data != null) return seat.of(data).closeDevice(fd);
    _ = c.close(fd);
}

const interface = c.struct_libinput_interface{
    .open_restricted = openRestricted,
    .close_restricted = closeRestricted,
};

fn in_of(p: ?*anyopaque) *Input {
    return @ptrCast(@alignCast(p.?));
}

/// Start reading input from every device on the seat. `seat_handle` is from
/// pancake_seat_open, or NULL to open devices directly on "seat0".
export fn pancake_input_open(seat_handle: ?*anyopaque) ?*anyopaque {
    const udev = c.udev_new() orelse return null;
    const li = c.libinput_udev_create_context(&interface, seat_handle, udev) orelse {
        _ = c.udev_unref(udev);
        return null;
    };
    const seat_name: [*c]const u8 = if (seat_handle != null) seat.of(seat_handle).name() else "seat0";
    if (c.libinput_udev_assign_seat(li, seat_name) != 0) {
        _ = c.libinput_unref(li);
        _ = c.udev_unref(udev);
        return null;
    }
    const in = std.heap.c_allocator.create(Input) catch return null;
    in.* = .{ .udev = udev, .li = li };
    return in;
}

/// File descriptor to poll() for readability before calling next.
export fn pancake_input_fd(handle: ?*anyopaque) c_int {
    return c.libinput_get_fd(in_of(handle).li);
}

/// Fill `out` with the next event. Returns 1 if there was one, 0 if not.
export fn pancake_input_next(handle: ?*anyopaque, out: *Event) c_int {
    const li = in_of(handle).li;
    _ = c.libinput_dispatch(li);
    while (c.libinput_get_event(li)) |ev| {
        defer c.libinput_event_destroy(ev);
        if (translate(ev, out)) return 1;
    }
    return 0;
}

/// Release all input devices (the seat was disabled: VT switched away).
export fn pancake_input_suspend(handle: ?*anyopaque) void {
    c.libinput_suspend(in_of(handle).li);
}

/// Reopen input devices after the seat comes back.
export fn pancake_input_resume(handle: ?*anyopaque) c_int {
    return c.libinput_resume(in_of(handle).li);
}

export fn pancake_input_close(handle: ?*anyopaque) void {
    if (handle == null) return;
    const in = in_of(handle);
    _ = c.libinput_unref(in.li);
    _ = c.udev_unref(in.udev);
    std.heap.c_allocator.destroy(in);
}

fn translate(ev: *c.struct_libinput_event, out: *Event) bool {
    out.* = std.mem.zeroes(Event);
    switch (c.libinput_event_get_type(ev)) {
        c.LIBINPUT_EVENT_DEVICE_ADDED, c.LIBINPUT_EVENT_DEVICE_REMOVED => |t| {
            out.kind = if (t == c.LIBINPUT_EVENT_DEVICE_ADDED) Kind.device_added else Kind.device_removed;
            const dev = c.libinput_event_get_device(ev);
            const name = std.mem.span(c.libinput_device_get_name(dev));
            const n = @min(name.len, out.name.len - 1);
            @memcpy(out.name[0..n], name[0..n]);
        },
        c.LIBINPUT_EVENT_KEYBOARD_KEY => {
            const k = c.libinput_event_get_keyboard_event(ev);
            out.kind = Kind.key;
            out.time_usec = c.libinput_event_keyboard_get_time_usec(k);
            out.code = c.libinput_event_keyboard_get_key(k);
            out.pressed = @intFromBool(c.libinput_event_keyboard_get_key_state(k) == c.LIBINPUT_KEY_STATE_PRESSED);
        },
        c.LIBINPUT_EVENT_POINTER_MOTION => {
            const p = c.libinput_event_get_pointer_event(ev);
            out.kind = Kind.motion;
            out.time_usec = c.libinput_event_pointer_get_time_usec(p);
            out.x = c.libinput_event_pointer_get_dx(p);
            out.y = c.libinput_event_pointer_get_dy(p);
        },
        c.LIBINPUT_EVENT_POINTER_MOTION_ABSOLUTE => {
            // Tablets and VMs: position as a 0..1 fraction of the screen.
            const p = c.libinput_event_get_pointer_event(ev);
            out.kind = Kind.motion_absolute;
            out.time_usec = c.libinput_event_pointer_get_time_usec(p);
            out.x = c.libinput_event_pointer_get_absolute_x_transformed(p, 1_000_000) / 1_000_000.0;
            out.y = c.libinput_event_pointer_get_absolute_y_transformed(p, 1_000_000) / 1_000_000.0;
        },
        c.LIBINPUT_EVENT_POINTER_BUTTON => {
            const p = c.libinput_event_get_pointer_event(ev);
            out.kind = Kind.button;
            out.time_usec = c.libinput_event_pointer_get_time_usec(p);
            out.code = c.libinput_event_pointer_get_button(p);
            out.pressed = @intFromBool(c.libinput_event_pointer_get_button_state(p) == c.LIBINPUT_BUTTON_STATE_PRESSED);
        },
        c.LIBINPUT_EVENT_POINTER_AXIS => {
            const p = c.libinput_event_get_pointer_event(ev);
            out.kind = Kind.scroll;
            out.time_usec = c.libinput_event_pointer_get_time_usec(p);
            if (c.libinput_event_pointer_has_axis(p, c.LIBINPUT_POINTER_AXIS_SCROLL_HORIZONTAL) != 0)
                out.x = c.libinput_event_pointer_get_axis_value(p, c.LIBINPUT_POINTER_AXIS_SCROLL_HORIZONTAL);
            if (c.libinput_event_pointer_has_axis(p, c.LIBINPUT_POINTER_AXIS_SCROLL_VERTICAL) != 0)
                out.y = c.libinput_event_pointer_get_axis_value(p, c.LIBINPUT_POINTER_AXIS_SCROLL_VERTICAL);
        },
        else => return false,
    }
    return true;
}
