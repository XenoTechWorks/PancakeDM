//! PancakeDM low-level layer: DRM/KMS, libinput, buffers, seat handling.
//! Exposed to the Rust compositor through a C ABI (include/pancake_low.h).

const std = @import("std");

comptime {
    _ = @import("input.zig");
    _ = @import("vt.zig");
}
const c = @cImport({
    @cInclude("xf86drm.h");
    @cInclude("xf86drmMode.h");
    @cInclude("fcntl.h");
    @cInclude("sys/mman.h");
    @cInclude("unistd.h");
});

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
    fb_id: u32 = 0,
    handle: u32 = 0,
    pitch: u32 = 0,
    size: u64 = 0,
    map: ?[*]u8 = null,
    cursor: [2]CursorBuf = .{ .{}, .{} },
    /// Which cursor buffer is on screen; the other one is safe to draw into.
    cursor_front: usize = 0,
    hot_x: i32 = 0,
    hot_y: i32 = 0,
};

const CursorBuf = struct {
    handle: u32 = 0,
    pitch: u32 = 0,
    size: u64 = 0,
    map: ?[*]u8 = null,
};

const Device = struct {
    fd: c_int,
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

/// Open a DRM device (NULL = /dev/dri/card0) and find connected outputs.
export fn pancake_low_open(path: ?[*:0]const u8) ?*anyopaque {
    const p: [*:0]const u8 = path orelse "/dev/dri/card0";
    const fd = c.open(p, c.O_RDWR | c.O_CLOEXEC);
    if (fd < 0) return null;
    const dev = std.heap.c_allocator.create(Device) catch {
        _ = c.close(fd);
        return null;
    };
    dev.* = .{ .fd = fd };
    // Fails if another compositor holds the GPU; probing still works then.
    _ = c.drmSetMaster(fd);
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

/// Fill an output with one XRGB8888 color and show it.
/// Returns 0, or <0: -1 bad index, -2 buffer setup failed, -3 modeset failed
/// (usually: another compositor owns the GPU; run from a TTY).
export fn pancake_low_fill(handle: ?*anyopaque, index: u32, xrgb: u32) c_int {
    const dev = dev_of(handle);
    if (index >= dev.count) return -1;
    const out = &dev.outputs[index];
    if (out.map == null) createFb(dev.fd, out) catch return -2;

    const px: [*]u32 = @ptrCast(@alignCast(out.map.?));
    const w: usize = out.mode.hdisplay;
    const stride: usize = out.pitch / 4;
    var y: usize = 0;
    while (y < out.mode.vdisplay) : (y += 1) {
        @memset((px + y * stride)[0..w], xrgb);
    }

    var conn = out.connector_id;
    if (c.drmModeSetCrtc(dev.fd, out.crtc_id, out.fb_id, 0, 0, &conn, 1, &out.mode) != 0)
        return -3;
    return 0;
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
        if (out.map) |m| _ = c.munmap(m, out.size);
        if (out.fb_id != 0) _ = c.drmModeRmFB(dev.fd, out.fb_id);
        if (out.handle != 0) _ = c.drmModeDestroyDumbBuffer(dev.fd, out.handle);
    }
    _ = c.drmDropMaster(dev.fd);
    _ = c.close(dev.fd);
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

fn createCursorBuf(dev: *Device, cb: *CursorBuf) !void {
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

fn createFb(fd: c_int, out: *Output) !void {
    const w = out.mode.hdisplay;
    const h = out.mode.vdisplay;
    if (c.drmModeCreateDumbBuffer(fd, w, h, 32, 0, &out.handle, &out.pitch, &out.size) != 0)
        return error.CreateDumb;
    if (c.drmModeAddFB(fd, w, h, 24, 32, out.pitch, out.handle, &out.fb_id) != 0)
        return error.AddFb;
    var offset: u64 = 0;
    if (c.drmModeMapDumbBuffer(fd, out.handle, &offset) != 0)
        return error.MapDumb;
    const p = c.mmap(null, out.size, c.PROT_READ | c.PROT_WRITE, c.MAP_SHARED, fd, @intCast(offset));
    if (p == null or @intFromPtr(p) == std.math.maxInt(usize)) return error.Mmap;
    out.map = @ptrCast(p);
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
