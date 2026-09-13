//! The retry schedule.
//!
//! An endpoint that fails is usually failing for everyone at once — a deploy,
//! a database, an outage. Retrying on a fixed delay turns that into a
//! thundering herd that arrives every N seconds and keeps the endpoint down.
//! Retrying without a ceiling turns a day-long outage into a week of traffic.
//!
//! So: exponential growth, a cap, and **full jitter** — the delay is drawn
//! uniformly from `[0, backoff]` rather than being `backoff` exactly. Full
//! jitter spreads a herd of retries across the whole window instead of
//! bunching them at its edge, and it is the variant AWS measured as best for
//! both completion time and contention.

use rand::Rng;
use std::time::Duration;

/// How long to wait between attempts, and how many to make.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Schedule {
    /// Delay before the second attempt. The first is immediate.
    pub base: Duration,
    /// Multiplier applied per attempt.
    pub factor: f64,
    /// Ceiling on a single delay, before jitter.
    pub max_delay: Duration,
    /// Total attempts, the first included. Zero means never deliver.
    pub max_attempts: u32,
    /// Fraction of the delay that is randomized, from 0.0 (none) to 1.0 (full
    /// jitter, the delay drawn uniformly from zero to the computed backoff).
    pub jitter: f64,
}

impl Default for Schedule {
    /// Ten attempts spread over roughly twenty hours: seconds, then minutes,
    /// then hours. Long enough to ride out a deploy, a bad release or an
    /// overnight outage, short enough that a dead endpoint is given up on the
    /// next day rather than retried forever.
    fn default() -> Schedule {
        Schedule {
            base: Duration::from_secs(5),
            factor: 4.0,
            max_delay: Duration::from_secs(6 * 60 * 60),
            max_attempts: 10,
            jitter: 1.0,
        }
    }
}

impl Schedule {
    /// The delay before attempt number `attempt`, counting the first as 1.
    ///
    /// Returns `None` when the schedule is exhausted, which is the caller's
    /// signal to mark the delivery failed rather than to wait.
    pub fn delay(&self, attempt: u32) -> Option<Duration> {
        self.delay_with(attempt, &mut rand::thread_rng())
    }

    /// [`Schedule::delay`] against a caller-supplied source of randomness, so
    /// a test can pin the jitter.
    pub fn delay_with<R: Rng>(&self, attempt: u32, rng: &mut R) -> Option<Duration> {
        if attempt == 0 {
            return Some(Duration::ZERO);
        }
        if attempt >= self.max_attempts {
            return None;
        }
        let growth = self.factor.powi(attempt as i32 - 1);
        let seconds = (self.base.as_secs_f64() * growth).min(self.max_delay.as_secs_f64());
        let jitter = self.jitter.clamp(0.0, 1.0);
        // `seconds * (1 - jitter)` is the floor; the rest is drawn uniformly.
        // At jitter 1.0 that is the full-jitter rule, at 0.0 it is none.
        let floor = seconds * (1.0 - jitter);
        let spread = seconds - floor;
        let chosen = if spread > 0.0 {
            floor + rng.gen_range(0.0..=spread)
        } else {
            floor
        };
        Some(Duration::from_secs_f64(chosen.max(0.0)))
    }

    /// Every delay the schedule can produce, at maximum, for documentation and
    /// for the endpoint detail page.
    pub fn worst_case(&self) -> Vec<Duration> {
        (1..self.max_attempts)
            .map(|attempt| {
                let growth = self.factor.powi(attempt as i32 - 1);
                let seconds = (self.base.as_secs_f64() * growth).min(self.max_delay.as_secs_f64());
                Duration::from_secs_f64(seconds)
            })
            .collect()
    }

    /// How long a delivery can stay alive in the worst case.
    pub fn worst_case_total(&self) -> Duration {
        self.worst_case().iter().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::mock::StepRng;

    fn fixed() -> StepRng {
        // Always returns the same value, so `gen_range` lands at the bottom of
        // whatever range it is given.
        StepRng::new(0, 0)
    }

    #[test]
    fn the_first_attempt_is_immediate() {
        assert_eq!(Schedule::default().delay(0), Some(Duration::ZERO));
    }

    #[test]
    fn the_schedule_ends() {
        let s = Schedule {
            max_attempts: 3,
            ..Schedule::default()
        };
        assert!(s.delay(1).is_some());
        assert!(s.delay(2).is_some());
        assert_eq!(
            s.delay(3),
            None,
            "a schedule of three has no fourth attempt"
        );
        assert_eq!(s.delay(99), None);
    }

    #[test]
    fn a_schedule_of_one_never_retries() {
        let s = Schedule {
            max_attempts: 1,
            ..Schedule::default()
        };
        assert_eq!(s.delay(0), Some(Duration::ZERO));
        assert_eq!(s.delay(1), None);
    }

    #[test]
    fn delays_grow_and_then_stop_growing() {
        let s = Schedule {
            base: Duration::from_secs(1),
            factor: 2.0,
            max_delay: Duration::from_secs(16),
            max_attempts: 12,
            jitter: 0.0,
        };
        let seen: Vec<u64> = (1..9).map(|a| s.delay(a).unwrap().as_secs()).collect();
        assert_eq!(seen, vec![1, 2, 4, 8, 16, 16, 16, 16]);
    }

    #[test]
    fn full_jitter_spreads_across_the_whole_window() {
        // The point of full jitter: a herd of retries must not arrive at the
        // same instant. With the default schedule the fourth attempt is due
        // after ~320s, and the drawn delays should cover that range rather
        // than cluster at its end.
        let s = Schedule::default();
        let mut rng = rand::thread_rng();
        let mut buckets = [0usize; 4];
        for _ in 0..4_000 {
            let d = s.delay_with(4, &mut rng).unwrap().as_secs_f64();
            let ceiling = 5.0 * 4f64.powi(3);
            assert!(d <= ceiling + 1.0, "{} exceeded the backoff {}", d, ceiling);
            buckets[((d / ceiling * 4.0) as usize).min(3)] += 1;
        }
        for (i, count) in buckets.iter().enumerate() {
            assert!(*count > 600, "quarter {} got only {} of 4000", i, count);
        }
    }

    #[test]
    fn no_jitter_is_deterministic() {
        let s = Schedule {
            jitter: 0.0,
            ..Schedule::default()
        };
        let once = s.delay_with(3, &mut fixed());
        for _ in 0..50 {
            assert_eq!(s.delay(3), once);
        }
    }

    #[test]
    fn a_jitter_fraction_keeps_a_floor() {
        // Half jitter should never return less than half the backoff, which is
        // what someone tuning for latency rather than herd control wants.
        let s = Schedule {
            base: Duration::from_secs(100),
            factor: 1.0,
            jitter: 0.5,
            ..Schedule::default()
        };
        for _ in 0..2_000 {
            let d = s.delay(1).unwrap().as_secs_f64();
            assert!((50.0..=100.0).contains(&d), "{} is outside the window", d);
        }
    }

    #[test]
    fn the_default_rides_out_a_working_day() {
        let s = Schedule::default();
        let total = s.worst_case_total();
        assert!(
            total >= Duration::from_secs(12 * 60 * 60),
            "the default gives up after {:?}",
            total
        );
        assert!(
            total <= Duration::from_secs(48 * 60 * 60),
            "the default retries for {:?}, which is a week of noise for a dead endpoint",
            total
        );
        assert_eq!(s.worst_case().len(), 9, "ten attempts means nine waits");
    }
}
