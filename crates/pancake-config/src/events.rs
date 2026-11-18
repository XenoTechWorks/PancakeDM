//! Calendar events: a plain text file, one event per line:
//!
//! ```text
//! 2026-12-01 14:30 | Dentist
//! 2026-12-25 | Christmas
//! ```
//!
//! Lives in `~/.local/share/pancake/events`; edit it by hand or use the
//! `pancake-event` command.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
    pub title: String,
}

impl Event {
    /// When it starts (all-day events count from midnight).
    pub fn starts(&self) -> NaiveDateTime {
        self.date.and_time(self.time.unwrap_or(NaiveTime::MIN))
    }

    pub fn to_line(&self) -> String {
        match self.time {
            Some(t) => format!("{} {} | {}", self.date, t.format("%H:%M"), self.title),
            None => format!("{} | {}", self.date, self.title),
        }
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pancake/events")
}

pub fn parse_line(line: &str) -> Option<Event> {
    let (when, title) = line.split_once('|')?;
    let title = title.trim();
    let mut words = when.split_whitespace();
    let date = NaiveDate::parse_from_str(words.next()?, "%Y-%m-%d").ok()?;
    let time = match words.next() {
        Some(t) => Some(NaiveTime::parse_from_str(t, "%H:%M").ok()?),
        None => None,
    };
    (!title.is_empty() && words.next().is_none()).then(|| Event { date, time, title: title.to_string() })
}

/// All events, soonest first. Bad lines are skipped.
pub fn parse(text: &str) -> Vec<Event> {
    let mut v: Vec<Event> = text.lines().filter(|l| !l.trim_start().starts_with('#')).filter_map(parse_line).collect();
    v.sort_by_key(|e| e.starts());
    v
}

pub fn load() -> Vec<Event> {
    std::fs::read_to_string(path()).map(|t| parse(&t)).unwrap_or_default()
}

pub fn save(events: &[Event]) -> std::io::Result<()> {
    let p = path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut text = String::from("# PancakeDM events: DATE [HH:MM] | title\n");
    for e in events {
        text.push_str(&e.to_line());
        text.push('\n');
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, p)
}

/// Events that haven't ended: timed ones from `now`, all-day ones all day.
pub fn upcoming(events: &[Event], now: NaiveDateTime, limit: usize) -> Vec<Event> {
    events
        .iter()
        .filter(|e| if e.time.is_some() { e.starts() >= now } else { e.date >= now.date() })
        .take(limit)
        .cloned()
        .collect()
}

/// "today 14:30", "tomorrow", "Fri 5 Dec", "in 3 days".
pub fn describe(e: &Event, today: NaiveDate) -> String {
    let days = (e.date - today).num_days();
    let when = match days {
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        2..=6 => format!("{} ({}d)", e.date.format("%a"), days),
        _ => e.date.format("%a %-d %b").to_string(),
    };
    match e.time {
        Some(t) => format!("{when} {}", t.format("%H:%M")),
        None => when,
    }
}

/// "today", "tomorrow", "+3" (days), "YYYY-MM-DD", or "mon".."sun" (next one).
pub fn parse_date(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    let l = s.to_lowercase();
    match l.as_str() {
        "today" => return Some(today),
        "tomorrow" => return Some(today + Duration::days(1)),
        _ => {}
    }
    if let Some(n) = l.strip_prefix('+').and_then(|n| n.parse::<i64>().ok()) {
        return Some(today + Duration::days(n));
    }
    if let Some(i) = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].iter().position(|d| l.starts_with(d)) {
        let now = today.weekday().num_days_from_monday() as i64;
        let ahead = (i as i64 - now + 7) % 7;
        return Some(today + Duration::days(if ahead == 0 { 7 } else { ahead }));
    }
    NaiveDate::parse_from_str(&l, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn lines_round_trip_and_sort() {
        let ev = parse("# note\n2026-12-25 | Christmas\n2026-12-01 14:30 | Dentist\nbroken\n2026-13-01 | bad\n");
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].title, "Dentist");
        assert_eq!(ev[0].to_line(), "2026-12-01 14:30 | Dentist");
        assert_eq!(parse_line(&ev[1].to_line()), Some(ev[1].clone()));
    }

    #[test]
    fn upcoming_skips_the_past_but_keeps_today_all_day() {
        let ev = parse("2026-11-18 | All day\n2026-11-18 09:00 | Early\n2026-11-19 10:00 | Later\n");
        let now = d(2026, 11, 18).and_hms_opt(12, 0, 0).unwrap();
        let up = upcoming(&ev, now, 5);
        assert_eq!(up.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), vec!["All day", "Later"]);
    }

    #[test]
    fn friendly_dates() {
        let today = d(2026, 11, 18); // a Wednesday
        assert_eq!(parse_date("tomorrow", today), Some(d(2026, 11, 19)));
        assert_eq!(parse_date("+3", today), Some(d(2026, 11, 21)));
        assert_eq!(parse_date("fri", today), Some(d(2026, 11, 20)));
        assert_eq!(parse_date("wed", today), Some(d(2026, 11, 25)));
        assert_eq!(parse_date("2026-12-01", today), Some(d(2026, 12, 1)));
        assert_eq!(parse_date("nope", today), None);
        let e = parse_line("2026-11-19 14:30 | x").unwrap();
        assert_eq!(describe(&e, today), "tomorrow 14:30");
    }
}
