//! The circuit breaker: what to do about an endpoint that keeps failing.
//!
//! A retry schedule handles an endpoint that is briefly unwell. It handles an
//! endpoint that has been gone for a week much less well: every message queued
//! for it waits out the full schedule, and the queue fills with work that
//! cannot succeed. The breaker is the answer to the second case.
//!
//! Three states, and the middle one is the point. Closed is normal. Open means
//! recent attempts all failed, so nothing is sent for a while. Half-open is
//! what open decays into: one delivery is allowed through, and its result
//! decides whether the circuit closes or opens again. Without half-open a
//! breaker either never reopens or reopens into a thundering herd.

use std::time::Duration;

/// When to open the circuit, for how long, and when to give up entirely.
#[derive(Clone, Debug)]
pub struct Policy {
    /// Consecutive failed deliveries before the circuit opens.
    pub failures_to_open: u32,
    /// How long it stays open the first time.
    pub cooldown: Duration,
    /// The cooldown doubles with each further failure while open, up to this.
    pub max_cooldown: Duration,
    /// Consecutive failures after which the endpoint is disabled outright and
    /// a human has to turn it back on. `None` never disables.
    pub failures_to_disable: Option<u32>,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            // Five, not one: a single failure is a deploy, a restart, or a
            // network blip, and none of those should stop the queue.
            failures_to_open: 5,
            cooldown: Duration::from_secs(30),
            max_cooldown: Duration::from_secs(30 * 60),
            // Roughly a day of an endpoint failing everything.
            failures_to_disable: Some(200),
        }
    }
}

/// What to do after an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Carry on: the circuit stays closed.
    Continue,
    /// Stop sending to this endpoint until the given instant.
    Open { until: i64 },
    /// Switch the endpoint off. It will not come back without a human.
    Disable,
}

impl Policy {
    /// Decide what a failure means, given how many have happened in a row.
    ///
    /// `consecutive` counts this failure, so the first call passes 1.
    pub fn on_failure(&self, consecutive: u32, now: i64) -> Action {
        if let Some(limit) = self.failures_to_disable {
            if consecutive >= limit {
                return Action::Disable;
            }
        }
        if consecutive < self.failures_to_open {
            return Action::Continue;
        }
        Action::Open {
            until: now + self.cooldown_for(consecutive).as_millis() as i64,
        }
    }

    /// How long the circuit stays open after `consecutive` failures.
    ///
    /// It doubles per failure past the threshold, so an endpoint that has been
    /// gone for a day is probed every half hour rather than every thirty
    /// seconds, and one that failed five times is tried again promptly.
    pub fn cooldown_for(&self, consecutive: u32) -> Duration {
        let past = consecutive.saturating_sub(self.failures_to_open);
        // Saturating in the exponent as well as the product: 2^past overflows
        // long before the shifted cooldown reaches the cap.
        let factor = 1u64.checked_shl(past.min(32)).unwrap_or(u64::MAX);
        let millis = (self.cooldown.as_millis() as u64).saturating_mul(factor);
        Duration::from_millis(millis.min(self.max_cooldown.as_millis() as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn a_few_failures_do_not_open_the_circuit() {
        let p = Policy::default();
        for n in 1..p.failures_to_open {
            assert_eq!(
                p.on_failure(n, NOW),
                Action::Continue,
                "opened after {} failures",
                n
            );
        }
    }

    #[test]
    fn the_threshold_opens_it() {
        let p = Policy::default();
        let Action::Open { until } = p.on_failure(p.failures_to_open, NOW) else {
            panic!("the circuit should open at the threshold");
        };
        assert_eq!(until, NOW + p.cooldown.as_millis() as i64);
    }

    #[test]
    fn the_cooldown_grows_but_stops_at_the_cap() {
        let p = Policy::default();
        let mut last = Duration::ZERO;
        for n in p.failures_to_open..p.failures_to_open + 40 {
            let d = p.cooldown_for(n);
            assert!(d >= last, "the cooldown shrank at {} failures", n);
            assert!(
                d <= p.max_cooldown,
                "the cooldown passed its cap at {} failures",
                n
            );
            last = d;
        }
        assert_eq!(last, p.max_cooldown, "the cooldown should reach its cap");
    }

    #[test]
    fn a_hopeless_endpoint_is_disabled() {
        let p = Policy::default();
        let limit = p.failures_to_disable.expect("the default disables");
        assert_eq!(
            p.on_failure(limit - 1, NOW),
            Action::Open {
                until: NOW + p.max_cooldown.as_millis() as i64
            }
        );
        assert_eq!(p.on_failure(limit, NOW), Action::Disable);
        assert_eq!(p.on_failure(limit + 1_000, NOW), Action::Disable);
    }

    #[test]
    fn a_policy_that_never_disables_only_ever_opens() {
        let p = Policy {
            failures_to_disable: None,
            ..Policy::default()
        };
        assert!(matches!(p.on_failure(100_000, NOW), Action::Open { .. }));
    }

    #[test]
    fn an_absurd_failure_count_does_not_overflow() {
        let p = Policy {
            failures_to_disable: None,
            ..Policy::default()
        };
        assert_eq!(p.cooldown_for(u32::MAX), p.max_cooldown);
    }
}
