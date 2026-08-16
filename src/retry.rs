//! Bounded exponential-backoff helper (roadmap §33).
//!
//! The reconcile run-loop needs two independent backoff states: one for
//! WebSocket reconnect attempts, and one for DSP error retries. Both follow
//! the same doubling-and-cap algorithm, so this module provides a small,
//! testable `ExponentialBackoff` type rather than repeating the arithmetic
//! inline.
//!
//! # Usage
//!
//! ```rust
//! use std::time::Duration;
//! use picorecdsp::retry::ExponentialBackoff;
//!
//! let mut backoff = ExponentialBackoff::new(
//!     Duration::from_millis(100),  // initial
//!     Duration::from_secs(30),     // cap
//! );
//!
//! let d1 = backoff.next();   // 100 ms
//! let d2 = backoff.next();   // 200 ms
//! let d3 = backoff.next();   // 400 ms
//! backoff.reset();
//! let d4 = backoff.next();   // back to 100 ms
//! # let _ = (d1, d2, d3, d4);
//! ```

use std::time::Duration;

/// Bounded exponential backoff state.
///
/// Each call to [`next`][`ExponentialBackoff::next`] returns the current delay
/// and then doubles it, up to the configured cap. [`reset`][`ExponentialBackoff::reset`]
/// restores the initial delay (e.g. after a successful connection or operation).
#[derive(Debug, Clone)]
pub struct ExponentialBackoff {
    initial: Duration,
    cap: Duration,
    current: Duration,
}

impl ExponentialBackoff {
    /// Create a new backoff starting at `initial`, doubling on each step,
    /// and never exceeding `cap`.
    ///
    /// # Panics
    ///
    /// Panics if `initial` is zero or if `cap` is less than `initial`.
    pub fn new(initial: Duration, cap: Duration) -> Self {
        assert!(
            !initial.is_zero(),
            "ExponentialBackoff: initial must be > 0"
        );
        assert!(cap >= initial, "ExponentialBackoff: cap must be >= initial");
        Self {
            initial,
            cap,
            current: initial,
        }
    }

    /// Return the current delay without advancing.
    pub fn current(&self) -> Duration {
        self.current
    }

    /// Reset to the initial delay (call after a successful attempt).
    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

impl Iterator for ExponentialBackoff {
    type Item = Duration;

    /// Return the current delay and advance to the next (doubled, capped) value.
    ///
    /// This iterator is infinite — it never returns `None`.
    fn next(&mut self) -> Option<Duration> {
        let delay = self.current;
        self.current = (self.current * 2).min(self.cap);
        Some(delay)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_each_step() {
        let mut b = ExponentialBackoff::new(Duration::from_millis(100), Duration::from_secs(10));
        assert_eq!(b.next(), Some(Duration::from_millis(100)));
        assert_eq!(b.next(), Some(Duration::from_millis(200)));
        assert_eq!(b.next(), Some(Duration::from_millis(400)));
        assert_eq!(b.next(), Some(Duration::from_millis(800)));
    }

    #[test]
    fn caps_at_maximum() {
        let mut b = ExponentialBackoff::new(Duration::from_millis(100), Duration::from_millis(300));
        assert_eq!(b.next(), Some(Duration::from_millis(100)));
        assert_eq!(b.next(), Some(Duration::from_millis(200)));
        assert_eq!(b.next(), Some(Duration::from_millis(300)));
        assert_eq!(b.next(), Some(Duration::from_millis(300))); // stays at cap
    }

    #[test]
    fn reset_restores_initial() {
        let mut b = ExponentialBackoff::new(Duration::from_millis(100), Duration::from_secs(10));
        b.next();
        b.next();
        b.reset();
        assert_eq!(b.next(), Some(Duration::from_millis(100)));
    }

    #[test]
    fn current_does_not_advance() {
        let mut b = ExponentialBackoff::new(Duration::from_millis(50), Duration::from_secs(10));
        assert_eq!(b.current(), Duration::from_millis(50));
        assert_eq!(b.current(), Duration::from_millis(50)); // unchanged
        b.next();
        assert_eq!(b.current(), Duration::from_millis(100));
    }

    #[test]
    #[should_panic(expected = "initial must be > 0")]
    fn panics_on_zero_initial() {
        ExponentialBackoff::new(Duration::ZERO, Duration::from_secs(1));
    }

    #[test]
    #[should_panic(expected = "cap must be >= initial")]
    fn panics_when_cap_less_than_initial() {
        ExponentialBackoff::new(Duration::from_secs(2), Duration::from_secs(1));
    }
}
