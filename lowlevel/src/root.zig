//! PancakeDM low-level layer: DRM/KMS, libinput, buffers, seat handling.
//! Exposed to the Rust compositor through a C ABI (include/pancake_low.h).

const version_major = 0;
const version_minor = 1;
const version_patch = 0;

/// Packed as 0x00MMmmpp.
export fn pancake_low_version() u32 {
    return (version_major << 16) | (version_minor << 8) | version_patch;
}
