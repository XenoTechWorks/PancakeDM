//! pancake-event: add, list and remove calendar events.

use chrono::{Local, NaiveTime};
use pancake_config::events::{self, Event};

const USAGE: &str = "usage:
  pancake-event add DATE [HH:MM] TITLE...   DATE: YYYY-MM-DD, today, tomorrow, +3, mon..sun
  pancake-event list                        upcoming events
  pancake-event remove N                    remove the Nth event from 'list --all'
  pancake-event list --all                  every event, numbered

Events live in ~/.local/share/pancake/events (edit it by hand if you like).";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let now = Local::now().naive_local();
    let today = now.date();
    let mut all = events::load();
    match args.first().map(String::as_str) {
        Some("add") if args.len() >= 3 => {
            let Some(date) = events::parse_date(&args[1], today) else { return fail("can't read that date") };
            let (time, rest) = match args.get(2).and_then(|t| NaiveTime::parse_from_str(t, "%H:%M").ok()) {
                Some(t) => (Some(t), &args[3..]),
                None => (None, &args[2..]),
            };
            let title = rest.join(" ");
            if title.trim().is_empty() {
                return fail("give the event a title");
            }
            let e = Event { date, time, title };
            println!("Added: {}", events::describe(&e, today) + " · " + &e.title);
            all.push(e);
            all.sort_by_key(|e| e.starts());
            if let Err(e) = events::save(&all) {
                fail(&format!("can't save: {e}"));
            }
        }
        Some("list") => {
            let show = if args.get(1).map(String::as_str) == Some("--all") { all.clone() } else { events::upcoming(&all, now, 20) };
            if show.is_empty() {
                println!("Nothing coming up.");
            }
            for (i, e) in show.iter().enumerate() {
                println!("{:>3}. {:<16} {}", i + 1, events::describe(e, today), e.title);
            }
        }
        Some("remove") if args.len() == 2 => match args[1].parse::<usize>() {
            Ok(n) if n >= 1 && n <= all.len() => {
                let e = all.remove(n - 1);
                println!("Removed: {}", e.title);
                if let Err(e) = events::save(&all) {
                    fail(&format!("can't save: {e}"));
                }
            }
            _ => fail("no such event (numbers come from 'list --all')"),
        },
        _ => println!("{USAGE}"),
    }
}

fn fail(msg: &str) {
    eprintln!("pancake-event: {msg}");
    std::process::exit(1);
}
