//! pancake-fetch: a fastfetch-style system summary with a pancake.

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

/// A short stack, in 256-color ANSI: butter, syrup, pancakes, plate.
fn art() -> Vec<String> {
    let butter = "\x1b[38;5;229m";
    let syrup = "\x1b[38;5;130m";
    let cake = "\x1b[38;5;215m";
    let edge = "\x1b[38;5;172m";
    let plate = "\x1b[38;5;252m";
    vec![
        format!("        {butter}▄▄▄▄▄{RESET}         "),
        format!("    {syrup}▄▄▄▄█████▄▄▄▄{RESET}     "),
        format!("  {syrup}█████████████████{RESET}   "),
        format!("  {cake}██{syrup}█{cake}████{syrup}█{cake}████{syrup}█{cake}██{RESET}   "),
        format!("  {edge}▀▀▀{syrup}▀{edge}▀▀▀▀{syrup}█{edge}▀▀▀▀▀▀{RESET}   "),
        format!("  {cake}█████████████████{RESET}   "),
        format!("  {edge}▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀{RESET}   "),
        format!("  {cake}█████████████████{RESET}   "),
        format!("  {edge}▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀{RESET}   "),
        format!("{plate}▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀{RESET} "),
    ]
}

fn main() {
    let accent = "\x1b[38;5;215m";
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
        let a = art.get(i).cloned().unwrap_or_else(|| " ".repeat(22));
        let b = info.get(i).cloned().unwrap_or_default();
        println!("  {a}  {b}");
    }
    println!();
}
