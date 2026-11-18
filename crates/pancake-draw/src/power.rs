//! Low-power mode: on, off, or automatic (back off while the CPU is hot).

use pancake_config::{Config, LowPower};
use std::time::{Duration, Instant};

const HOT: f32 = 90.0;
const COOL: f32 = 82.0;

/// Decide with hysteresis, so it doesn't flap around the threshold.
pub fn decide(mode: LowPower, temp: Option<f32>, was_active: bool) -> bool {
    match mode {
        LowPower::On => true,
        LowPower::Off => false,
        LowPower::Auto => match temp {
            Some(t) if t >= HOT => true,
            Some(t) if t >= COOL => was_active,
            _ => false,
        },
    }
}

/// Tracks whether low-power mode is active, checking the temperature
/// every few seconds.
pub struct Power {
    active: bool,
    checked: Option<Instant>,
}

impl Power {
    pub fn new() -> Self {
        Self { active: false, checked: None }
    }

    pub fn active(&mut self, cfg: &Config) -> bool {
        if self.checked.map_or(true, |t| t.elapsed() > Duration::from_secs(3)) {
            self.checked = Some(Instant::now());
            let temp = if cfg.low_power == LowPower::Auto { pancake_fetch::temperature() } else { None };
            self.active = decide(cfg.low_power, temp, self.active);
        }
        self.active
    }
}

impl Default for Power {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hysteresis() {
        assert!(decide(LowPower::Auto, Some(95.0), false));
        assert!(decide(LowPower::Auto, Some(85.0), true)); // stays on until it cools
        assert!(!decide(LowPower::Auto, Some(85.0), false));
        assert!(!decide(LowPower::Auto, Some(70.0), true));
        assert!(!decide(LowPower::Auto, None, true));
        assert!(decide(LowPower::On, Some(30.0), false));
        assert!(!decide(LowPower::Off, Some(99.0), true));
    }
}
