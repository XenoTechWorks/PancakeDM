//! Which calendar events deserve a reminder right now.

use chrono::{Duration, NaiveDateTime, NaiveTime, Timelike};
use pancake_config::events::Event;
use std::collections::HashSet;

/// (key, summary, body). Timed events remind from 10 minutes before until
/// 5 minutes after the start; all-day events remind once, from 09:00.
/// `done` holds keys already shown; due ones are added to it.
pub fn due(events: &[Event], now: NaiveDateTime, done: &mut HashSet<String>) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for e in events {
        let key = e.to_line();
        if done.contains(&key) {
            continue;
        }
        let body = match e.time {
            Some(t) => {
                let until = e.starts() - now;
                if until > Duration::minutes(10) || until < -Duration::minutes(5) {
                    continue;
                }
                let at = t.format("%H:%M");
                match until.num_minutes() {
                    m if m > 0 => format!("In {m} min, at {at}"),
                    _ => format!("Starting now ({at})"),
                }
            }
            None => {
                if e.date != now.date() || now.time() < NaiveTime::from_hms_opt(9, 0, 0).unwrap() || now.time().hour() >= 23 {
                    continue;
                }
                "Today".to_string()
            }
        };
        done.insert(key.clone());
        out.push((key, e.title.clone(), body));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 18).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn timed_events_remind_once_inside_the_window() {
        let ev = pancake_config::events::parse("2026-11-18 14:30 | Dentist\n");
        let mut done = HashSet::new();
        assert!(due(&ev, at(14, 0), &mut done).is_empty()); // too early
        let r = due(&ev, at(14, 22), &mut done);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, "Dentist");
        assert_eq!(r[0].2, "In 8 min, at 14:30");
        assert!(due(&ev, at(14, 25), &mut done).is_empty()); // not twice
        let mut fresh = HashSet::new();
        assert!(due(&ev, at(15, 0), &mut fresh).is_empty()); // too late
        assert_eq!(due(&ev, at(14, 31), &mut fresh)[0].2, "Starting now (14:30)");
    }

    #[test]
    fn all_day_events_remind_from_nine() {
        let ev = pancake_config::events::parse("2026-11-18 | Birthday\n2026-11-19 | Tomorrow thing\n");
        let mut done = HashSet::new();
        assert!(due(&ev, at(8, 0), &mut done).is_empty());
        let r = due(&ev, at(9, 5), &mut done);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].2, "Today");
    }
}
