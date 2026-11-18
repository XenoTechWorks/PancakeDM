//! PancakeDM low-level layer: DRM/KMS, libinput, buffers, seat handling.
//! Exposed to the Rust compositor through a C ABI (include/pancake_low.h).

const std = @import("std");
const builtin = @import("builtin");

comptime {
    _ = @import("input.zig");
    _ = @import("vt.zig");
    _ = @import("seat.zig");
}
const c = @cImport({
    @cInclude("xf86drm.h");
    @cInclude("xf86drmMode.h");
    @cInclude("fcntl.h");
    @cInclude("sys/mman.h");
    @cInclude("unistd.h");
});

// Zig 0.13 spells the C calling convention .C; 0.14+ spells it .c.
const cc: std.builtin.CallingConvention =
    if (builtin.zig_version.order(.{ .major = 0, .minor = 14, .patch = 0 }) == .lt) .C else .c;

const version_major = 0;
const version_minor = 1;
const version_patch = 0;

/// Packed as 0x00MMmmpp.
export fn pancake_low_version() u32 {
    return (version_major << 16) | (version_minor << 8) | version_patch;
}

// ---- DRM/KMS ----------------------------------------------------------------

const max_outputs = 8;

const Output = struct {
    connector_id: u32,
    crtc_id: u32,
    mode: c.drmModeModeInfo,
    /// What the CRTC showed before us, restored on close.
    saved: [*c]c.drmModeCrtc,
    name: [32]u8,
    /// Two screen buffers: one on screen (front), one we draw into.
    bufs: [2]Buf = .{ .{}, .{} },
    front: usize = 0,
    /// A page flip is queued and the GPU hasn't swapped yet: don't draw.
    flip_pending: bool = false,
    /// Set by the flip-done event; collected by pancake_low_dispatch.
    flipped: bool = false,
    /// First frame, or after getting the VT back: do a full modeset.
    needs_modeset: bool = true,
    cursor: [2]Buf = .{ .{}, .{} },
    /// Which cursor buffer is on screen; the other one is safe to draw into.
    cursor_front: usize = 0,
    hot_x: i32 = 0,
    hot_y: i32 = 0,
};

/// A dumb buffer mapped into our memory. fb_id is 0 for cursor buffers.
const Buf = struct {
    fb_id: u32 = 0,
    handle: u32 = 0,
    pitch: u32 = 0,
    size: u64 = 0,
    map: ?[*]u8 = null,
};

const Device = struct {
    fd: c_int,
    /// False when the fd came from the seat (it closes it, and handles DRM master).
    owns_fd: bool = true,
    outputs: [max_outputs]Output = undefined,
    count: usize = 0,
    /// Hardware cursor size (DRM_CAP_CURSOR_WIDTH/HEIGHT, usually 64x64).
    cursor_w: u32 = 64,
    cursor_h: u32 = 64,
};

/// Matches `struct pancake_output_info` in pancake_low.h.
const OutputInfo = extern struct {
    width: u32,
    height: u32,
    refresh_mhz: u32,
    name: [32]u8,
};

fn dev_of(p: ?*anyopaque) *Device {
    return @ptrCast(@alignCast(p.?));
}

/// Open a DRM device (NULL = /dev/dri/card0) directly, without a seat.
export fn pancake_low_open(path: ?[*:0]const u8) ?*anyopaque {
    const p: [*:0]const u8 = path orelse "/dev/dri/card0";
    const fd = c.open(p, c.O_RDWR | c.O_CLOEXEC);
    if (fd < 0) return null;
    // Fails if another compositor holds the GPU; probing still works then.
    _ = c.drmSetMaster(fd);
    return setup(fd, true) orelse {
        _ = c.close(fd);
        return null;
    };
}

/// Use a DRM fd opened by the seat (pancake_seat_open_device). The seat
/// keeps ownership of the fd and of DRM master.
export fn pancake_low_open_fd(fd: c_int) ?*anyopaque {
    return setup(fd, false);
}

fn setup(fd: c_int, owns_fd: bool) ?*anyopaque {
    const dev = std.heap.c_allocator.create(Device) catch return null;
    dev.* = .{ .fd = fd, .owns_fd = owns_fd };
    probe(dev);
    var cap: u64 = 0;
    if (c.drmGetCap(fd, c.DRM_CAP_CURSOR_WIDTH, &cap) == 0 and cap > 0) dev.cursor_w = @intCast(cap);
    if (c.drmGetCap(fd, c.DRM_CAP_CURSOR_HEIGHT, &cap) == 0 and cap > 0) dev.cursor_h = @intCast(cap);
    return dev;
}

export fn pancake_low_output_count(handle: ?*anyopaque) u32 {
    return @intCast(dev_of(handle).count);
}

export fn pancake_low_output_info(handle: ?*anyopaque, index: u32, info: *OutputInfo) c_int {
    const dev = dev_of(handle);
    if (index >= dev.count) return -1;
    const out = &dev.outputs[index];
    const m = out.mode;
    var refresh: u64 = 0;
    if (m.htotal != 0 and m.vtotal != 0)
        refresh = @as(u64, m.clock) * 1_000_000 / (@as(u64, m.htotal) * m.vtotal);
    info.* = .{
        .width = m.hdisplay,
        .height = m.vdisplay,
        .refresh_mhz = @intCast(refresh),
        .name = out.name,
    };
    return 0;
}

// ---- Frames and page flipping --------------------------------------------------
//
// Each output has two buffers. We only ever draw into the one that isn't on
// screen, then ask the GPU to swap them at the next vblank (page flip). The
// GPU tells us through the DRM fd when the swap happened; until then the
// output is busy. Nothing is ever drawn into a visible buffer: no tearing.

/// Matches `struct pancake_frame` in pancake_low.h.
const Frame = extern struct {
    pixels: ?[*]u32,
    stride: u32, // in pixels
    width: u32,
    height: u32,
};

/// The DRM fd: poll it, and call pancake_low_dispatch when it's readable.
export fn pancake_low_fd(handle: ?*anyopaque) c_int {
    return dev_of(handle).fd;
}

/// Get the hidden buffer to draw the next frame into (XRGB8888).
/// 0 = `frame` filled in; 1 = busy, a flip is still pending; <0 = error.
export fn pancake_frame_begin(handle: ?*anyopaque, index: u32, frame: *Frame) c_int {
    const dev = dev_of(handle);
    if (index >= dev.count) return -1;
    const out = &dev.outputs[index];
    if (out.flip_pending) return 1;
    const back = &out.bufs[1 - out.front];
    if (back.map == null) createFb(dev.fd, out.mode, back) catch return -2;
    frame.* = .{
        .pixels = @ptrCast(@alignCast(back.map.?)),
        .stride = back.pitch / 4,
        .width = out.mode.hdisplay,
        .height = out.mode.vdisplay,
    };
    return 0;
}

/// Show the frame drawn since pancake_frame_begin.
/// 0 = flip queued (the output is busy until pancake_low_dispatch reports it);
/// 2 = shown right away (first frame / after reset: full modeset);
/// <0 = error (-3: the GPU refused, e.g. another compositor owns it).
export fn pancake_frame_commit(handle: ?*anyopaque, index: u32) c_int {
    const dev = dev_of(handle);
    if (index >= dev.count) return -1;
    const out = &dev.outputs[index];
    const back_i = 1 - out.front;
    const back = &out.bufs[back_i];
    if (back.fb_id == 0 or out.flip_pending) return -2;

    if (out.needs_modeset) {
        var conn = out.connector_id;
        if (c.drmModeSetCrtc(dev.fd, out.crtc_id, back.fb_id, 0, 0, &conn, 1, &out.mode) != 0)
            return -3;
        out.needs_modeset = false;
        out.front = back_i;
        return 2;
    }
    if (c.drmModePageFlip(dev.fd, out.crtc_id, back.fb_id, c.DRM_MODE_PAGE_FLIP_EVENT, out) != 0)
        return -3;
    out.flip_pending = true;
    // The old front is still on screen until the flip lands, but begin()
    // refuses while flip_pending, so nobody can draw into it early.
    out.front = back_i;
    return 0;
}

fn onPageFlip(_: c_int, _: c_uint, _: c_uint, _: c_uint, data: ?*anyopaque) callconv(cc) void {
    const out: *Output = @ptrCast(@alignCast(data.?));
    out.flip_pending = false;
    out.flipped = true;
}

/// Read DRM events (call when pancake_low_fd is readable). Returns a bitmask
/// of outputs whose flip just landed: they're free for the next frame.
export fn pancake_low_dispatch(handle: ?*anyopaque) u32 {
    const dev = dev_of(handle);
    var ctx = std.mem.zeroes(c.drmEventContext);
    ctx.version = 2;
    ctx.page_flip_handler = onPageFlip;
    _ = c.drmHandleEvent(dev.fd, &ctx);
    var mask: u32 = 0;
    for (dev.outputs[0..dev.count], 0..) |*out, i| {
        if (out.flipped) {
            mask |= @as(u32, 1) << @as(u5, @intCast(i));
            out.flipped = false;
        }
    }
    return mask;
}

/// After getting the VT back: flips queued before the switch will never
/// land, and another session may have changed the screens. Forget pending
/// flips; the next commit on each output does a full modeset.
export fn pancake_low_reset(handle: ?*anyopaque) void {
    const dev = dev_of(handle);
    for (dev.outputs[0..dev.count]) |*out| {
        out.flip_pending = false;
        out.flipped = false;
        out.needs_modeset = true;
    }
}

/// Restore what each screen showed before, free everything, close the GPU.
export fn pancake_low_close(handle: ?*anyopaque) void {
    if (handle == null) return;
    const dev = dev_of(handle);
    for (dev.outputs[0..dev.count]) |*out| {
        _ = c.drmModeSetCursor(dev.fd, out.crtc_id, 0, 0, 0);
        for (&out.cursor) |*cb| {
            if (cb.map) |m| _ = c.munmap(m, cb.size);
            if (cb.handle != 0) _ = c.drmModeDestroyDumbBuffer(dev.fd, cb.handle);
        }
        if (out.saved != null) {
            const s = out.saved;
            if (s.*.mode_valid != 0) {
                var conn = out.connector_id;
                _ = c.drmModeSetCrtc(dev.fd, s.*.crtc_id, s.*.buffer_id, s.*.x, s.*.y, &conn, 1, &s.*.mode);
            }
            c.drmModeFreeCrtc(s);
        }
        for (&out.bufs) |*b| {
            if (b.map) |m| _ = c.munmap(m, b.size);
            if (b.fb_id != 0) _ = c.drmModeRmFB(dev.fd, b.fb_id);
            if (b.handle != 0) _ = c.drmModeDestroyDumbBuffer(dev.fd, b.handle);
        }
    }
    if (dev.owns_fd) {
        _ = c.drmDropMaster(dev.fd);
        _ = c.close(dev.fd);
    }
    std.heap.c_allocator.destroy(dev);
}

// ---- Hardware cursor ----------------------------------------------------------
//
// The cursor lives on the GPU's cursor plane, never in the screen image, so
// moving it can't leave trails or smears behind. Avoiding the other classic
// cursor glitches:
//   - junk around the icon: the plane buffer (e.g. 64x64) is bigger than the
//     icon, so the whole buffer is cleared before every upload;
//   - tearing on shape change: two buffers, we only draw into the one that
//     isn't on screen, then swap;
//   - hotspot drift: the position we move to is always (x - hot, y - hot).

/// Hardware cursor size for this GPU, so callers can size icons.
export fn pancake_cursor_size(handle: ?*anyopaque, w: *u32, h: *u32) void {
    const dev = dev_of(handle);
    w.* = dev.cursor_w;
    h.* = dev.cursor_h;
}

/// Show a premultiplied ARGB8888 image (w*h pixels, tightly packed) as the
/// cursor on an output. Returns 0, -1 bad index, -2 buffer setup failed,
/// -3 GPU has no cursor plane, -4 image bigger than pancake_cursor_size.
export fn pancake_cursor_set(handle: ?*anyopaque, index: u32, pixels: [*]const u32, w: u32, h: u32, hot_x: i32, hot_y: i32) c_int {
    const dev = dev_of(handle);
    if (index >= dev.count) return -1;
    if (w > dev.cursor_w or h > dev.cursor_h) return -4;
    const out = &dev.outputs[index];
    const back = 1 - out.cursor_front;
    const cb = &out.cursor[back];
    if (cb.map == null) createCursorBuf(dev, cb) catch return -2;

    const dst: [*]u32 = @ptrCast(@alignCast(cb.map.?));
    const stride: usize = cb.pitch / 4;
    @memset(dst[0 .. stride * dev.cursor_h], 0);
    for (0..h) |y| {
        @memcpy((dst + y * stride)[0..w], (pixels + y * w)[0..w]);
    }

    if (c.drmModeSetCursor2(dev.fd, out.crtc_id, cb.handle, dev.cursor_w, dev.cursor_h, hot_x, hot_y) != 0 and
        c.drmModeSetCursor(dev.fd, out.crtc_id, cb.handle, dev.cursor_w, dev.cursor_h) != 0)
        return -3;
    out.cursor_front = back;
    out.hot_x = hot_x;
    out.hot_y = hot_y;
    return 0;
}

/// Move the cursor so its hotspot sits at (x, y) on the output.
export fn pancake_cursor_move(handle: ?*anyopaque, index: u32, x: i32, y: i32) c_int {
    const dev = dev_of(handle);
    if (index >= dev.count) return -1;
    const out = &dev.outputs[index];
    return c.drmModeMoveCursor(dev.fd, out.crtc_id, x - out.hot_x, y - out.hot_y);
}

/// Hide the cursor on an output.
export fn pancake_cursor_hide(handle: ?*anyopaque, index: u32) c_int {
    const dev = dev_of(handle);
    if (index >= dev.count) return -1;
    return c.drmModeSetCursor(dev.fd, dev.outputs[index].crtc_id, 0, 0, 0);
}

fn createCursorBuf(dev: *Device, cb: *Buf) !void {
    if (c.drmModeCreateDumbBuffer(dev.fd, dev.cursor_w, dev.cursor_h, 32, 0, &cb.handle, &cb.pitch, &cb.size) != 0)
        return error.CreateDumb;
    var offset: u64 = 0;
    if (c.drmModeMapDumbBuffer(dev.fd, cb.handle, &offset) != 0) return error.MapDumb;
    const p = c.mmap(null, cb.size, c.PROT_READ | c.PROT_WRITE, c.MAP_SHARED, dev.fd, @intCast(offset));
    if (p == null or @intFromPtr(p) == std.math.maxInt(usize)) return error.Mmap;
    cb.map = @ptrCast(p);
}

fn probe(dev: *Device) void {
    const res = c.drmModeGetResources(dev.fd);
    if (res == null) return;
    defer c.drmModeFreeResources(res);

    var used_crtcs: u32 = 0;
    for (0..@as(usize, @intCast(res.*.count_connectors))) |i| {
        if (dev.count >= max_outputs) break;
        const conn = c.drmModeGetConnector(dev.fd, res.*.connectors[i]);
        if (conn == null) continue;
        defer c.drmModeFreeConnector(conn);
        if (conn.*.connection != c.DRM_MODE_CONNECTED or conn.*.count_modes == 0) continue;

        var mode = conn.*.modes[0];
        for (0..@as(usize, @intCast(conn.*.count_modes))) |m| {
            if ((conn.*.modes[m].type & c.DRM_MODE_TYPE_PREFERRED) != 0) {
                mode = conn.*.modes[m];
                break;
            }
        }
        const crtc_id = findCrtc(dev.fd, res, conn, &used_crtcs) orelse continue;

        var out = Output{
            .connector_id = conn.*.connector_id,
            .crtc_id = crtc_id,
            .mode = mode,
            .saved = c.drmModeGetCrtc(dev.fd, crtc_id),
            .name = std.mem.zeroes([32]u8),
        };
        _ = std.fmt.bufPrint(out.name[0..31], "{s}-{d}", .{
            connectorName(conn.*.connector_type), conn.*.connector_type_id,
        }) catch {};
        dev.outputs[dev.count] = out;
        dev.count += 1;
    }
}

fn findCrtc(fd: c_int, res: [*c]c.drmModeRes, conn: [*c]c.drmModeConnector, used: *u32) ?u32 {
    for (0..@as(usize, @intCast(conn.*.count_encoders))) |e| {
        const enc = c.drmModeGetEncoder(fd, conn.*.encoders[e]);
        if (enc == null) continue;
        defer c.drmModeFreeEncoder(enc);
        for (0..@as(usize, @intCast(res.*.count_crtcs))) |ci| {
            if (ci >= 32) break;
            const bit = @as(u32, 1) << @as(u5, @intCast(ci));
            if ((enc.*.possible_crtcs & bit) != 0 and (used.* & bit) == 0) {
                used.* |= bit;
                return res.*.crtcs[ci];
            }
        }
    }
    return null;
}

fn createFb(fd: c_int, mode: c.drmModeModeInfo, b: *Buf) !void {
    const w = mode.hdisplay;
    const h = mode.vdisplay;
    if (c.drmModeCreateDumbBuffer(fd, w, h, 32, 0, &b.handle, &b.pitch, &b.size) != 0)
        return error.CreateDumb;
    if (c.drmModeAddFB(fd, w, h, 24, 32, b.pitch, b.handle, &b.fb_id) != 0)
        return error.AddFb;
    var offset: u64 = 0;
    if (c.drmModeMapDumbBuffer(fd, b.handle, &offset) != 0)
        return error.MapDumb;
    const p = c.mmap(null, b.size, c.PROT_READ | c.PROT_WRITE, c.MAP_SHARED, fd, @intCast(offset));
    if (p == null or @intFromPtr(p) == std.math.maxInt(usize)) return error.Mmap;
    b.map = @ptrCast(p);
    // Start black, not with whatever was in that memory.
    @memset(b.map.?[0..@intCast(b.size)], 0);
}

fn connectorName(t: u32) []const u8 {
    return switch (t) {
        c.DRM_MODE_CONNECTOR_VGA => "VGA",
        c.DRM_MODE_CONNECTOR_DVII, c.DRM_MODE_CONNECTOR_DVID, c.DRM_MODE_CONNECTOR_DVIA => "DVI",
        c.DRM_MODE_CONNECTOR_DisplayPort => "DP",
        c.DRM_MODE_CONNECTOR_HDMIA, c.DRM_MODE_CONNECTOR_HDMIB => "HDMI",
        c.DRM_MODE_CONNECTOR_LVDS => "LVDS",
        c.DRM_MODE_CONNECTOR_eDP => "eDP",
        c.DRM_MODE_CONNECTOR_VIRTUAL => "Virtual",
        c.DRM_MODE_CONNECTOR_DSI => "DSI",
        else => "Unknown",
    };
}
