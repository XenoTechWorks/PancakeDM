const std = @import("std");
const builtin = @import("builtin");

// Supports Zig 0.13 (addStaticLibrary) and 0.14+ (addLibrary), since distros
// ship different versions.
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const lib = if (comptime builtin.zig_version.order(.{ .major = 0, .minor = 14, .patch = 0 }) == .lt)
        b.addStaticLibrary(.{
            .name = "pancake_low",
            .root_source_file = b.path("src/root.zig"),
            .target = target,
            .optimize = optimize,
            .link_libc = true,
        })
    else
        b.addLibrary(.{
            .name = "pancake_low",
            .linkage = .static,
            .root_module = b.createModule(.{
                .root_source_file = b.path("src/root.zig"),
                .target = target,
                .optimize = optimize,
                .link_libc = true,
            }),
        });
    // libdrm keeps drm.h in its own folder; override for odd layouts.
    const drm_inc = b.option([]const u8, "drm-include", "libdrm include dir") orelse "/usr/include/libdrm";
    lib.addSystemIncludePath(.{ .cwd_relative = drm_inc });
    lib.linkSystemLibrary("drm");
    lib.linkSystemLibrary("input");
    lib.linkSystemLibrary("udev");
    // Rust links without unwinding/sanitizer runtimes from Zig.
    lib.bundle_compiler_rt = true;
    b.installArtifact(lib);
    lib.installHeader(b.path("include/pancake_low.h"), "pancake_low.h");
}
