//! The wall-clock, and the only file in this crate allowed to read it.
//!
//! # Why it is one file
//!
//! `ARCHITECTURE.md` Rule 1: the simulation advances by tick, never by elapsed
//! seconds. `scripts/check-architecture.sh` enforces that by grepping the
//! simulation crates for `Instant::now`, and this crate is one of them — with
//! this file named as the exception.
//!
//! Something must eventually turn seconds into ticks, or a dedicated server
//! would run the world as fast as the CPU allows. The client does it in
//! `main.rs` (`docs/PHASE1_ARCHITECTURE.md` §9); this is the server's equivalent,
//! kept to one file so the exception is a place rather than a habit. The check
//! names `clock.rs` explicitly, so a second file reaching for the clock fails
//! CI rather than quietly becoming precedent.
//!
//! Nothing downstream of [`Pacer`] sees a duration. It hands out **tick counts**,
//! and a tick count is what the simulation understands.

use std::time::{Duration, Instant};

/// Paces a loop at a fixed tick rate, in whole ticks.
///
/// Sleeps out the remainder of each tick rather than spinning: a dedicated
/// server sharing a host with anything else must not burn a core to stay idle,
/// which is what a busy-wait at 60 Hz is.
pub struct Pacer {
    period: Duration,
    /// When the next tick is due. Advanced by exactly one period per tick, so
    /// scheduling error does not accumulate the way `now + period` would.
    next: Instant,
    /// Ticks the clock owed that were deliberately not run. Reported rather
    /// than silently absorbed, because a server that cannot keep up is
    /// something an operator needs to be told about.
    pub dropped: u64,
}

/// Past this many ticks behind, the backlog is abandoned rather than chased.
///
/// The same spiral-of-death guard the client has, for the same reason: catching
/// up costs more than real time, which makes the next backlog bigger. A server
/// that falls behind should run slow and say so, not lock up.
const MAX_CATCH_UP: u64 = 20;

impl Pacer {
    /// A pacer running at `tps` ticks per second.
    pub fn new(tps: u32) -> Self {
        let period = Duration::from_secs_f64(1.0 / tps.max(1) as f64);
        Self {
            period,
            next: Instant::now(),
            dropped: 0,
        }
    }

    /// Block until at least one tick is due, then report how many are owed.
    ///
    /// Never returns zero, so a caller cannot busy-loop on it.
    pub fn wait(&mut self) -> u64 {
        let now = Instant::now();
        if let Some(remaining) = self.next.checked_duration_since(now) {
            std::thread::sleep(remaining);
        }

        let now = Instant::now();
        let mut owed = 1;
        self.next += self.period;
        while self.next <= now {
            self.next += self.period;
            owed += 1;
        }

        if owed > MAX_CATCH_UP {
            self.dropped += owed - MAX_CATCH_UP;
            // Restart the schedule from now rather than from a deadline far in
            // the past, which would keep reporting a backlog forever.
            self.next = now + self.period;
            owed = MAX_CATCH_UP;
        }
        owed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pacer_never_reports_zero_ticks() {
        let mut p = Pacer::new(1000);
        for _ in 0..5 {
            assert!(p.wait() >= 1);
        }
    }

    /// A pacer whose deadline is far in the past must not hand back a
    /// thousand-tick backlog -- that is the spiral this guard exists to stop.
    #[test]
    fn a_long_stall_is_dropped_rather_than_chased() {
        let mut p = Pacer::new(60);
        p.next = Instant::now() - Duration::from_secs(60); // an hour of ticks
        assert_eq!(p.wait(), MAX_CATCH_UP, "the backlog is capped");
        assert!(p.dropped > 3000, "and the drop is reported: {}", p.dropped);

        // And the schedule really restarted: the next wait is an ordinary one.
        assert!(p.wait() <= MAX_CATCH_UP);
    }

    /// 60 ticks at 1000 tps is 60ms of sleeping, not 60ms of spinning. Asserted
    /// as elapsed time rather than CPU time (which is not portable), so this
    /// checks the pacing itself: it must take roughly the time it promises
    /// rather than returning instantly.
    #[test]
    fn a_pacer_actually_paces() {
        let mut p = Pacer::new(1000);
        let start = Instant::now();
        let mut ticks = 0;
        while ticks < 50 {
            ticks += p.wait();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(40),
            "50 ticks at 1000 tps cannot take {elapsed:?}"
        );
    }
}
