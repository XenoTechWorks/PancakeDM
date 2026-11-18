//! PancakeDM compositor. Window management and policy are written in Rust;
//! the low-level layer (DRM/KMS, input, buffers) is Zig, under `lowlevel/`.

mod low;

use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "\
usage: pancake-compositor [options]
  --list             list connected screens and exit
  --fill RRGGBB      fill every screen with a color (run from a TTY)
  --seconds N        how long to show the fill (default 5)
  --device PATH      DRM device (default /dev/dri/card0)
  --version          print versions and exit";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut list, mut fill, mut seconds, mut device) = (false, None, 5u64, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--list" => list = true,
            "--fill" => match args.next().and_then(|v| u32::from_str_radix(v.trim_start_matches('#'), 16).ok()) {
                Some(c) => fill = Some(c),
                None => return fail("--fill needs a color like f4a259"),
            },
            "--seconds" => match args.next().and_then(|v| v.parse().ok()) {
                Some(s) => seconds = s,
                None => return fail("--seconds needs a number"),
            },
            "--device" => device = args.next(),
            "--version" => {
                println!("pancake-compositor {}", env!("CARGO_PKG_VERSION"));
                println!("low-level layer: {}", low::version());
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => return fail(&format!("unknown option {other}\n{USAGE}")),
        }
    }
    if !list && fill.is_none() {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let dev = match low::Device::open(device.as_deref()) {
        Ok(d) => d,
        Err(e) => return fail(&e),
    };
    let outputs = dev.outputs();
    if outputs.is_empty() {
        return fail("no connected screens found");
    }
    for (i, o) in outputs.iter().enumerate() {
        println!(
            "{i}: {} {}x{} @ {}.{:03} Hz",
            o.name, o.width, o.height, o.refresh_mhz / 1000, o.refresh_mhz % 1000
        );
    }

    if let Some(color) = fill {
        for i in 0..outputs.len() as u32 {
            if let Err(e) = dev.fill(i, color) {
                return fail(&e);
            }
        }
        std::thread::sleep(Duration::from_secs(seconds));
        // Dropping `dev` puts the old screen contents back.
    }
    ExitCode::SUCCESS
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("pancake-compositor: {msg}");
    ExitCode::FAILURE
}
