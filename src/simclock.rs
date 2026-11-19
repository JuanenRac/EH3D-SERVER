//! The clock of the simulated machines: it counts the seconds a job ran, stops while the job is held and can be made to run faster than real time.
//!
//! The plugins that have no real machine behind (`laser`, `pnp`, `pcb`) all keep the time of a job this way, so a job takes as long as its work would
//! (divided by the rate of the machine) and a test can set the rate to 0 to have it end at once.

use std::time::Instant;

/// The seconds a simulated job has run, with the instant it last started running.
#[derive(Debug, Clone, Default)]
pub struct SimClock {
    /// The seconds it ran before the last hold.
    ran_s: f64,
    since: Option<Instant>,
}

impl SimClock {
    /// A clock that is stopped at zero.
    pub fn new() -> SimClock {
        SimClock::default()
    }

    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    /// Starts or goes on counting; a clock that already runs is not touched.
    pub fn run(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }

    /// Stops counting and keeps what ran.
    pub fn hold(&mut self) {
        self.ran_s = self.real_s();
        self.since = None;
    }

    /// The real seconds it has counted.
    pub fn real_s(&self) -> f64 {
        self.ran_s + self.since.map_or(0.0, |t| t.elapsed().as_secs_f64())
    }

    /// The seconds of work done in the time that ran, at `rate` times real time. A rate of 0 means no waiting: the work is all done as soon as the clock runs.
    pub fn work_s(&self, rate: f64) -> f64 {
        if rate <= 0.0 {
            if self.running() || self.ran_s > 0.0 {
                f64::INFINITY
            } else {
                0.0
            }
        } else {
            self.real_s() * rate
        }
    }

    /// Sets the seconds that ran (for the tests, which cannot wait).
    pub fn set_ran_s(&mut self, seconds: f64) {
        self.ran_s = seconds.max(0.0);
        self.since = None;
    }
}

/// How many of the items whose cumulative times are `cumulative` (non-decreasing, in seconds) are finished after `work_s` seconds.
pub fn finished_items(cumulative: &[f64], work_s: f64) -> usize {
    cumulative.partition_point(|t| *t <= work_s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_clock_keeps_what_ran_and_a_stopped_one_has_done_nothing() {
        let mut c = SimClock::new();
        assert!(!c.running());
        assert_eq!(c.work_s(1.0), 0.0);
        assert_eq!(c.work_s(0.0), 0.0);
        c.run();
        assert!(c.running());
        c.hold();
        assert!(!c.running());
        let kept = c.real_s();
        assert!(kept >= 0.0);
        assert_eq!(c.real_s(), kept);
    }

    #[test]
    fn the_rate_scales_the_work_and_zero_means_all_at_once() {
        let mut c = SimClock::new();
        c.set_ran_s(3.0);
        assert!((c.work_s(2.0) - 6.0).abs() < 1e-9);
        assert!(c.work_s(0.0).is_infinite());
        c.set_ran_s(-5.0);
        assert_eq!(c.real_s(), 0.0);
        let mut running = SimClock::new();
        running.run();
        assert!(running.work_s(0.0).is_infinite());
    }

    #[test]
    fn the_finished_items_are_counted_from_their_cumulative_times() {
        let t = [1.0, 2.0, 2.0, 5.0];
        assert_eq!(finished_items(&t, 0.5), 0);
        assert_eq!(finished_items(&t, 1.0), 1);
        assert_eq!(finished_items(&t, 2.0), 3);
        assert_eq!(finished_items(&t, 4.9), 3);
        assert_eq!(finished_items(&t, f64::INFINITY), 4);
        assert_eq!(finished_items(&[], 3.0), 0);
    }
}
