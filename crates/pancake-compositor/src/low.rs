//! Bindings to the Zig low-level layer (see `lowlevel/include/pancake_low.h`).

#[cfg(feature = "zig-backend")]
mod ffi {
    extern "C" {
        pub fn pancake_low_version() -> u32;
    }
}

#[cfg(feature = "zig-backend")]
pub fn version() -> String {
    let v = unsafe { ffi::pancake_low_version() };
    format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
}

#[cfg(not(feature = "zig-backend"))]
pub fn version() -> String {
    "not linked (build with --features zig-backend)".into()
}
