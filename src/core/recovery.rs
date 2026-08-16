//! Shared retry/backoff and config-fingerprint logic.
//!
//! These types are used by both the aloop state machine (via the WebSocket
//! controller) and the ioplug controller loop so that the two backends
//! implement identical recovery semantics.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

// ─── Retry/backoff state ───────────────────────────────────────────────────

/// Exponential-backoff state for CamillaDSP restart attempts.
///
/// `latch_until_change` is set for permanent errors (e.g. config validation
/// failures) where retrying without a config change would be pointless.
/// It is cleared when the config file fingerprint changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectionReason {
    Config,
    PlaybackDevice,
    Internal,
}

pub struct RetryState {
    /// How many start attempts have been made since the last reset.
    pub consecutive: u32,
    /// Earliest time the next attempt may be made.
    next_at: Option<Instant>,
    /// Permanent failure latch — cleared by a config file change.
    pub latch_until_change: bool,
    last_rejection_reason: Option<RejectionReason>,
}

const RETRY_DELAYS_MS: &[u64] = &[500, 1000, 2000, 5000, 10_000, 30_000];

impl RetryState {
    pub fn new() -> Self {
        Self {
            consecutive: 0,
            next_at: None,
            latch_until_change: false,
            last_rejection_reason: None,
        }
    }

    pub fn reset_backoff(&mut self) {
        self.consecutive = 0;
        self.next_at = None;
        if !self.latch_until_change {
            self.last_rejection_reason = None;
        }
    }

    pub fn clear_latch(&mut self) {
        self.latch_until_change = false;
        if self.next_at.is_none() {
            self.last_rejection_reason = None;
        }
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
        self.record_attempt_with_reason(RejectionReason::Internal);
    }

    pub fn record_attempt_with_reason(&mut self, reason: RejectionReason) {
        let delay =
            RETRY_DELAYS_MS[self.consecutive.min((RETRY_DELAYS_MS.len() - 1) as u32) as usize];
        self.next_at = Some(Instant::now() + Duration::from_millis(delay));
        self.consecutive += 1;
        self.last_rejection_reason = Some(reason);
    }

    /// Duration scheduled by the most recent `record_attempt` call.
    pub fn scheduled_delay(&self) -> Option<Duration> {
        if self.consecutive == 0 {
            None
        } else {
            Some(Duration::from_millis(
                RETRY_DELAYS_MS
                    [(self.consecutive - 1).min((RETRY_DELAYS_MS.len() - 1) as u32) as usize],
            ))
        }
    }

    /// Mark a permanent error; no further attempts until the latch clears.
    pub fn latch(&mut self) {
        self.latch_with_reason(RejectionReason::Internal);
    }

    pub fn latch_with_reason(&mut self, reason: RejectionReason) {
        self.latch_until_change = true;
        self.last_rejection_reason = Some(reason);
    }

    pub fn rejection_reason(&self) -> RejectionReason {
        self.last_rejection_reason
            .unwrap_or(RejectionReason::Internal)
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
    target_modified: Option<SystemTime>,
    target_size: u64,
    target_ino: u64,
    path_modified: Option<SystemTime>,
    path_size: u64,
    path_ino: u64,
    symlink_target: Option<PathBuf>,
}

impl ConfigFingerprint {
    /// Return a fingerprint that compares not-equal to any real file.
    pub fn absent() -> Self {
        Self {
            target: PathBuf::new(),
            target_modified: None,
            target_size: 0,
            target_ino: 0,
            path_modified: None,
            path_size: 0,
            path_ino: 0,
            symlink_target: None,
        }
    }

    /// Sample the current state of `path` (may be a symlink).
    pub fn sample(path: &Path) -> Self {
        let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path_meta = fs::symlink_metadata(path).ok();
        let target_meta = fs::metadata(path).ok();
        Self {
            target,
            target_modified: target_meta.as_ref().and_then(|m| m.modified().ok()),
            target_size: target_meta.as_ref().map_or(0, |m| m.len()),
            target_ino: target_meta.as_ref().map_or(0, MetadataExt::ino),
            path_modified: path_meta.as_ref().and_then(|m| m.modified().ok()),
            path_size: path_meta.as_ref().map_or(0, |m| m.len()),
            path_ino: path_meta.as_ref().map_or(0, MetadataExt::ino),
            symlink_target: fs::read_link(path).ok(),
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
    fn retry_state_reports_scheduled_delay_sequence() {
        let mut r = RetryState::new();
        let expected_ms = [500, 1000, 2000, 5000, 10_000, 30_000, 30_000];
        for expected in expected_ms {
            r.record_attempt();
            assert_eq!(
                r.scheduled_delay(),
                Some(Duration::from_millis(expected)),
                "unexpected scheduled delay after {} attempts",
                r.consecutive
            );
        }
    }

    #[test]
    fn retry_state_latch_prevents_attempt() {
        let mut r = RetryState::new();
        r.latch();
        assert!(!r.should_attempt());
    }

    #[test]
    fn retry_state_reset_backoff_keeps_permanent_latch() {
        let mut r = RetryState::new();
        r.latch();
        r.record_attempt();
        r.reset_backoff();
        assert!(!r.should_attempt());
        assert_eq!(r.consecutive, 0);
        assert!(r.latch_until_change);
    }

    #[test]
    fn retry_state_clear_latch_requires_separate_call() {
        let mut r = RetryState::new();
        r.latch();
        r.record_attempt();
        r.reset_backoff();
        r.clear_latch();
        assert!(r.should_attempt());
        assert_eq!(r.consecutive, 0);
        assert!(!r.latch_until_change);
    }

    #[test]
    fn retry_state_tracks_last_rejection_reason() {
        let mut r = RetryState::new();
        r.record_attempt_with_reason(RejectionReason::PlaybackDevice);
        assert_eq!(r.rejection_reason(), RejectionReason::PlaybackDevice);
        // Not latched, so `reset_backoff()` also forgets the transient
        // rejection reason; `rejection_reason()` falls back to its
        // `Internal` default rather than reporting `PlaybackDevice` again.
        r.reset_backoff();
        assert_eq!(r.rejection_reason(), RejectionReason::Internal);
        r.latch_with_reason(RejectionReason::Config);
        assert_eq!(r.rejection_reason(), RejectionReason::Config);
        // Once latched, `reset_backoff()` must NOT forget the reason behind
        // the still-active permanent latch (only `clear_latch()` may do so).
        r.reset_backoff();
        assert_eq!(r.rejection_reason(), RejectionReason::Config);
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

    #[test]
    fn fingerprint_detects_broken_symlink_retarget() {
        let dir = std::env::temp_dir();
        let link = dir.join("picoredsp-fp-broken-link.txt");
        let missing_a = dir.join("picoredsp-fp-missing-a.txt");
        let missing_b = dir.join("picoredsp-fp-missing-b.txt");
        let tmp_link = dir.join("picoredsp-fp-broken-link-tmp.txt");

        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&tmp_link);
        std::os::unix::fs::symlink(&missing_a, &link).unwrap();
        let fp1 = ConfigFingerprint::sample(&link);

        std::os::unix::fs::symlink(&missing_b, &tmp_link).unwrap();
        std::fs::rename(&tmp_link, &link).unwrap();

        let fp2 = ConfigFingerprint::sample(&link);
        assert_ne!(fp1, fp2, "broken symlink retarget must be detected");
    }
}
