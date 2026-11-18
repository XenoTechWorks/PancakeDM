//! PancakeDM compositor. Window management and policy are written in Rust;
//! the low-level layer (DRM/KMS, input, buffers) is Zig, under `lowlevel/`.

mod low;

fn main() {
    println!("pancake-compositor {}", env!("CARGO_PKG_VERSION"));
    println!("low-level layer: {}", low::version());
    println!("config: {}", pancake_config::config_dir().display());
    println!("ipc: {}", pancake_ipc::socket_path().display());
}
