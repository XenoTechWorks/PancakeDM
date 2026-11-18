fn main() {
    if std::env::var_os("CARGO_FEATURE_ZIG_BACKEND").is_some() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lowlevel/zig-out/lib");
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=pancake_low");
        println!("cargo:rustc-link-lib=dylib=drm");
        println!("cargo:rustc-link-lib=dylib=input");
        println!("cargo:rustc-link-lib=dylib=udev");
        println!("cargo:rustc-link-lib=dylib=seat");
        println!("cargo:rerun-if-changed=../../lowlevel/src");
    }
}
