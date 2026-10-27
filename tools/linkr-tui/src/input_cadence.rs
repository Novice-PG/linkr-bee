//! Short receive polling after input without speeding up idle animations.
use std::time::{Duration, Instant};

const IDLE: Duration = Duration::from_millis(250);
const INTERACTIVE: Duration = Duration::from_millis(20);
const ECHO_WINDOW: Duration = Duration::from_secs(1);

pub struct InputCadence {
    echo_until: Instant,
    next_tick: Instant,
}

impl InputCadence {
    pub fn new(now: Instant) -> Self {
        Self {
            echo_until: now,
            next_tick: now + IDLE,
        }
    }

    pub fn input(&mut self, now: Instant) {
        self.echo_until = now + ECHO_WINDOW;
    }

    pub fn wait(&self, now: Instant) -> Duration {
        let idle = self.next_tick.saturating_duration_since(now);
        if now < self.echo_until {
            idle.min(INTERACTIVE)
        } else {
            idle
        }
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        if now < self.next_tick {
            return false;
        }
        self.next_tick = now + IDLE;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_echo_is_serviced_promptly_even_without_another_key() {
        let start = Instant::now();
        let mut cadence = InputCadence::new(start);
        cadence.input(start);
        // Simulated radio arrivals after the last key. Only the keyboard can
        // wake poll, so an echo must fit inside the receive polling budget.
        for arrival_ms in [1, 31, 67, 125, 249, 401, 905] {
            let arrival = start + Duration::from_millis(arrival_ms);
            let mut now = start;
            while now < arrival {
                cadence.tick(now);
                now += cadence.wait(now);
            }
            assert!(now.duration_since(arrival) <= INTERACTIVE);
        }
    }

    #[test]
    fn fast_polling_preserves_animation_cadence_and_returns_to_idle() {
        let start = Instant::now();
        let mut cadence = InputCadence::new(start);
        assert_eq!(cadence.wait(start), IDLE);
        cadence.input(start);
        let mut ticks = 0;
        for ms in 0..=1000 {
            if cadence.tick(start + Duration::from_millis(ms)) {
                ticks += 1;
            }
        }
        assert_eq!(ticks, 4);
        assert_eq!(cadence.wait(start + ECHO_WINDOW), IDLE);
        cadence.input(start + ECHO_WINDOW);
        assert_eq!(cadence.wait(start + ECHO_WINDOW), INTERACTIVE);
    }
}
