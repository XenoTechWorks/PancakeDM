//! pancake-fetch: a fastfetch-style system summary for the installed OS.

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

/// A compact distro mark. This deliberately uses terminal text instead of a
/// generated Pancake logo; the OS name beside it is the source of truth.
fn art() -> Vec<String> {
    let id = pancake_fetch::os_id().unwrap_or_else(|| "linux".into());
    let (mark, color) = match id.as_str() {
        "arch" | "endeavouros" | "manjaro" => ("ARCH", 39),
        "ubuntu" | "pop" | "elementary" => ("UBUNTU", 202),
        "debian" => ("DEBIAN", 160),
        "fedora" | "rhel" | "centos" => ("FEDORA", 33),
        "opensuse" | "opensuse-tumbleweed" => ("SUSE", 40),
        "void" => ("VOID", 35),
        "alpine" => ("ALPINE", 25),
        "kiss" => ("KISS", 214),
        "nixos" => ("NIXOS", 75),
        _ => ("LINUX", 244),
    };
    let ink = format!("\x1b[38;5;{color}m");
    let width = 16usize;
    let left = (width.saturating_sub(mark.len())) / 2;
    let right = width.saturating_sub(mark.len() + left);
    vec![
        format!("{ink}╭────────────────╮{RESET}"),
        format!("{ink}│                │{RESET}"),
        format!("{ink}│{}{}{}│{RESET}", " ".repeat(left), mark, " ".repeat(right)),
        format!("{ink}│                │{RESET}"),
        format!("{ink}╰────────────────╯{RESET}"),
    ]
}

fn main() {
    let accent = "\x1b[38;5;75m";
    let mut info: Vec<String> = Vec::new();
    let user = pancake_fetch::user().unwrap_or_else(|| "user".into());
    let host = pancake_fetch::hostname().unwrap_or_else(|| "pancake".into());
    let title = format!("{user}@{host}");
    info.push(format!("{BOLD}{accent}{title}{RESET}"));
    info.push("─".repeat(title.chars().count()));
    for (k, v) in pancake_fetch::gather() {
        info.push(format!("{BOLD}{accent}{k}{RESET}: {v}"));
    }
    info.push(String::new());
    // Color blocks, like every good fetch.
    info.push((0..8).map(|i| format!("\x1b[4{i}m   ")).collect::<String>() + RESET);
    info.push((0..8).map(|i| format!("\x1b[10{i}m   ")).collect::<String>() + RESET);

    let art = art();
    let rows = art.len().max(info.len());
    println!();
    for i in 0..rows {
        let a = art.get(i).cloned().unwrap_or_else(|| " ".repeat(18));
        let b = info.get(i).cloned().unwrap_or_default();
        println!("  {a}  {b}");
    }
    println!();
}
