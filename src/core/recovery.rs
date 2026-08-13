//! Shared retry/backoff and config-fingerprint logic.
//!
//! These types are used by both the aloop state machine (via the WebSocket
//! controller) and the ioplug controller loop so that the two backends
//! implement identical recovery semantics.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

// ─── Retry/backoff state ───────────────────────────────────────────────────

/// Exponential-backoff state for CamillaDSP restart attempts.
///
/// `latch_until_change` is set for permanent errors (e.g. config validation
/// failures) where retrying without a config change would be pointless.
/// It is cleared when the config file fingerprint changes.
pub struct RetryState {
    /// How many start attempts have been made since the last reset.
    pub consecutive: u32,
    /// Earliest time the next attempt may be made.
    next_at: Option<Instant>,
    /// Permanent failure latch — cleared by a config file change.
    pub latch_until_change: bool,
}

impl RetryState {
    pub fn new() -> Self {
        Self {
            consecutive: 0,
            next_at: None,
            latch_until_change: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Returns `true` if enough time has elapsed since the last attempt and
    /// the permanent latch is not set.
    pub fn should_attempt(&self) -> bool {
        if self.latch_until_change {
            return false;
        }
        self.next_at.map(|t| Instant::now() >= t).unwrap_or(true)
    }

    /// Record a start attempt, setting the next backoff window.
    ///
    /// Backoff sequence: 500 ms → 1 s → 2 s → 5 s → 10 s → 30 s (cap).
    pub fn record_attempt(&mut self) {
        const DELAYS_MS: &[u64] = &[500, 1000, 2000, 5000, 10_000, 30_000];
        let delay = DELAYS_MS[self.consecutive.min(5) as usize];
        self.next_at = Some(Instant::now() + Duration::from_millis(delay));
        self.consecutive += 1;
    }

    /// Mark a permanent error; no further attempts until the latch clears.
    pub fn latch(&mut self) {
        self.latch_until_change = true;
    }
}

impl Default for RetryState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Config fingerprint ────────────────────────────────────────────────────

/// Lightweight fingerprint for detecting active config file changes without
/// polling the entire YAML content.
///
/// Tracks the canonicalized symlink target (catches CamillaGUI config
/// switches), the target file's mtime/size (catches in-place edits), and the
/// inode number (distinguishes files with identical size and visible mtime).
#[derive(Debug, Eq, PartialEq)]
pub struct ConfigFingerprint {
    target: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
    ino: u64,
}

impl ConfigFingerprint {
    /// Return a fingerprint that compares not-equal to any real file.
    pub fn absent() -> Self {
        Self {
            target: PathBuf::new(),
            modified: None,
            size: 0,
            ino: 0,
        }
    }

    /// Sample the current state of `path` (may be a symlink).
    pub fn sample(path: &PathBuf) -> Self {
        let target = path.canonicalize().unwrap_or_else(|_| path.clone());
        let meta = fs::metadata(path);
        let modified = meta.as_ref().ok().and_then(|m| m.modified().ok());
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let ino = meta.as_ref().map(|m| m.ino()).unwrap_or(0);
        Self {
            target,
            modified,
            size,
            ino,
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RetryState ──────────────────────────────────────────────────────────

    #[test]
    fn retry_state_new_allows_immediate_attempt() {
        let r = RetryState::new();
        assert!(r.should_attempt());
    }

    #[test]
    fn retry_state_record_attempt_sets_backoff() {
        let mut r = RetryState::new();
        r.record_attempt();
        // Immediately after recording, backoff window is active.
        assert!(!r.should_attempt());
    }

    #[test]
    fn retry_state_backoff_sequence_grows() {
        let mut r = RetryState::new();
        // Record 6 attempts and observe the consecutive counter.
        for i in 1..=6 {
            r.record_attempt();
            assert_eq!(r.consecutive, i);
        }
    }

    #[test]
    fn retry_state_latch_prevents_attempt() {
        let mut r = RetryState::new();
        r.latch();
        assert!(!r.should_attempt());
    }

    #[test]
    fn retry_state_reset_clears_latch_and_backoff() {
        let mut r = RetryState::new();
        r.latch();
        r.record_attempt();
        r.reset();
        assert!(r.should_attempt());
        assert_eq!(r.consecutive, 0);
        assert!(!r.latch_until_change);
    }

    #[test]
    fn retry_state_default_allows_immediate_attempt() {
        let r = RetryState::default();
        assert!(r.should_attempt());
    }

    // ── ConfigFingerprint ───────────────────────────────────────────────────

    #[test]
    fn fingerprint_absent_differs_from_real_file() {
        let path = std::env::temp_dir().join("picoredsp-fp-test.txt");
        std::fs::write(&path, "x").unwrap();
        let fp = ConfigFingerprint::sample(&path);
        assert!(fp != ConfigFingerprint::absent());
    }

    #[test]
    fn fingerprint_same_file_is_equal() {
        let path = std::env::temp_dir().join("picoredsp-fp-same.txt");
        std::fs::write(&path, "hello").unwrap();
        let fp1 = ConfigFingerprint::sample(&path);
        let fp2 = ConfigFingerprint::sample(&path);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_detects_content_change() {
        let path = std::env::temp_dir().join("picoredsp-fp-change.txt");
        std::fs::write(&path, "before").unwrap();
        let fp1 = ConfigFingerprint::sample(&path);
        std::fs::write(&path, "after_with_more_bytes").unwrap();
        let fp2 = ConfigFingerprint::sample(&path);
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn fingerprint_detects_symlink_retarget() {
        // Simulate a CamillaGUI config switch: symlink initially points to v1,
        // then is atomically retargeted to v2 (different content, different inode).
        let dir = std::env::temp_dir();
        let v1 = dir.join("picoredsp-fp-retarget-v1.txt");
        let v2 = dir.join("picoredsp-fp-retarget-v2.txt");
        let link = dir.join("picoredsp-fp-retarget-link.txt");

        std::fs::write(&v1, "config version 1 has this content").unwrap();
        std::fs::write(&v2, "config version 2 has different content").unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&v1, &link).unwrap();

        let fp1 = ConfigFingerprint::sample(&link);

        // Retarget the symlink atomically.
        let tmp_link = dir.join("picoredsp-fp-retarget-tmp.txt");
        let _ = std::fs::remove_file(&tmp_link);
        std::os::unix::fs::symlink(&v2, &tmp_link).unwrap();
        std::fs::rename(&tmp_link, &link).unwrap();

        let fp2 = ConfigFingerprint::sample(&link);
        assert_ne!(fp1, fp2, "symlink retarget must be detected");
    }
}
