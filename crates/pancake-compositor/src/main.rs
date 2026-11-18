//! PancakeDM compositor. Window management and policy are written in Rust;
//! the low-level layer (DRM/KMS, input, buffers) is Zig, under `lowlevel/`.

mod low;

use low::{key, InputEvent};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: pancake-compositor [options]
  --list             list connected screens and exit
  --input            print keyboard/mouse events (Esc quits)
  --fill RRGGBB      fill every screen with a color (run from a TTY);
                     Space cycles colors, Esc quits
  --seconds N        stop after N seconds (default 60, 0 = no limit)
  --device PATH      DRM device (default /dev/dri/card0)
  --version          print versions and exit";

/// Colors Space cycles through in --fill.
const PALETTE: [u32; 4] = [0xf4a259, 0x8cb369, 0x5b8e7d, 0xbc4b51];

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut list, mut input, mut fill, mut seconds, mut device) = (false, false, None, 60u64, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--list" => list = true,
            "--input" => input = true,
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
    let deadline = (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds));

    if input && fill.is_none() {
        return match run_input(deadline) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
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
        if let Err(e) = run_fill(&dev, outputs.len() as u32, color, deadline) {
            return fail(&e);
        }
        // Dropping `dev` puts the old screen contents back.
    }
    ExitCode::SUCCESS
}

fn run_fill(dev: &low::Device, outputs: u32, color: u32, deadline: Option<Instant>) -> Result<(), String> {
    let paint = |c: u32| (0..outputs).try_for_each(|i| dev.fill(i, c));
    paint(color)?;
    let input = low::Input::open()?;
    let mut next = 0;
    while !expired(deadline) {
        if !input.wait(100) {
            continue;
        }
        while let Some(ev) = input.next() {
            match ev {
                InputEvent::Key { code: key::ESC, pressed: true } => return Ok(()),
                InputEvent::Key { code: key::SPACE, pressed: true } => {
                    paint(PALETTE[next])?;
                    next = (next + 1) % PALETTE.len();
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn run_input(deadline: Option<Instant>) -> Result<(), String> {
    let input = low::Input::open()?;
    while !expired(deadline) {
        if !input.wait(100) {
            continue;
        }
        while let Some(ev) = input.next() {
            println!("{ev:?}");
            if ev == (InputEvent::Key { code: key::ESC, pressed: true }) {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|d| Instant::now() >= d)
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("pancake-compositor: {msg}");
    ExitCode::FAILURE
}
