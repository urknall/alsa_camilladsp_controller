//! [`SourceObserver`] trait and [`SourceSnapshot`] — Cliffhanger B workaround
//! (roadmap §19).
//!
//! Today Rust must observe `snd-aloop` via ALSA HCTL to determine whether a
//! producer is active and what its sample rate is.  This code lives exclusively
//! in this module so it can be deleted as a single unit when CamillaDSP upstream
//! takes over source-detection.
//!
//! # Removal criterion (Cliffhanger B)
//!
//! Registered in `upstream/capabilities.yml` under the key
//! `native_aloop_rate_following`.  Deletion condition:
//!
//! > Once CamillaDSP upstream reliably detects loopback active, reads the
//! > current source rate itself, processes rate changes itself, releases capture
//! > on inactive, and starts the new rate itself → `source/alsa_loopback.rs`
//! > (and this file) → **DELETE**.  If CamillaDSP takes over the complete
//! > lifecycle → `rate_sync/` + large parts of `reconcile.rs` → **DELETE**, and
//! > subsequently the Rust daemon itself may potentially be deleted.
//!
//! # ALSA HCTL implementation note
//!
//! The real implementation opens `snd-aloop` via the ALSA HCTL API (non-blocking),
//! subscribes to element-change events, debounces for ~50 ms, and re-reads a full
//! snapshot afterward.  It is gated behind `#[cfg(target_os = "linux")]` because
//! the ALSA library is only available on Linux.  On other platforms (CI, macOS
//! dev machines) the trait can still be implemented with mock/test doubles.

use async_trait::async_trait;

use crate::{error::PicorecdspError, source::SourceState};

// ── SourceSnapshot ────────────────────────────────────────────────────────────

/// A fresh, point-in-time snapshot of everything Rust reads from `snd-aloop`
/// (roadmap §8, State Truth 1 — Source Transport State).
///
/// This is always read fresh: Rust never invents these values and never caches
/// them across reconcile triggers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    /// Whether a producer is holding the loopback open and streaming.
    pub state: SourceState,

    /// The nominal sample rate as reported by `snd-aloop`, if a producer is
    /// active.  This is the same value carried by `SourceState::Active`, but
    /// also available here for pattern-matching convenience.
    pub sample_rate: Option<u32>,

    /// The actually negotiated PCM format (e.g. `"S32_LE"`).  Used only for
    /// transport-invariant checks; Rust never changes the format.
    pub format: Option<String>,

    /// The actually negotiated channel count.  Used only for transport-invariant
    /// checks; Rust never changes the channel count.
    pub channels: Option<u32>,

    /// A monotonically increasing generation counter.  Even if a new producer
    /// starts at the same sample rate as the previous one, the generation
    /// changes — allowing the reconciler to detect new-source-same-rate events
    /// (roadmap §31).
    pub generation: u64,
}

impl SourceSnapshot {
    /// Return `true` if a producer is currently active.
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }
}

// ── SourceObserver trait ───────────────────────────────────────────────────────

/// The only way Rust is allowed to learn about the source state (roadmap §5,
/// §11, §19 / Cliffhanger B).
///
/// Implementations must:
/// * Provide a `snapshot()` that returns a fresh HCTL read every time (no
///   caching).
/// * Provide a `next_trigger()` that blocks until `snd-aloop` signals an
///   element-change event — debounced for ~50 ms.  The event payload itself is
///   never treated as truth; the reconciler always calls `snapshot()` afterward.
///
/// # Removal criterion (see module doc)
///
/// `upstream/capabilities.yml` key: `native_aloop_rate_following`.
#[async_trait]
pub trait SourceObserver: Send {
    /// Return a fresh point-in-time snapshot of `snd-aloop` state.
    async fn snapshot(&self) -> Result<SourceSnapshot, PicorecdspError>;

    /// Block until `snd-aloop` fires an HCTL element-change event, apply a
    /// ~50 ms debounce, and return.  The caller must call `snapshot()` after
    /// this returns to get the actual settled state.
    ///
    /// Returns `Ok(())` normally.  Returns `Err` if the HCTL handle is
    /// invalidated (e.g. the ALSA device was unloaded).
    async fn next_trigger(&mut self) -> Result<(), PicorecdspError>;
}

// ── Tests (mock-based, no real ALSA) ─────────────────────────────────────────

#[cfg(test)]
pub mod testing {
    //! Test doubles for [`SourceObserver`].

    use super::*;
    use std::sync::{Arc, Mutex};

    /// A controllable mock [`SourceObserver`] for reconciler unit tests.
    ///
    /// The test can push snapshots into the observer and fire triggers to wake
    /// a waiting `next_trigger()` call.
    pub struct MockSourceObserver {
        snapshots: Arc<Mutex<Vec<SourceSnapshot>>>,
        trigger_tx: tokio::sync::mpsc::UnboundedSender<()>,
        trigger_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>,
    }

    impl MockSourceObserver {
        pub fn new() -> Self {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            Self {
                snapshots: Arc::new(Mutex::new(vec![])),
                trigger_tx: tx,
                trigger_rx: tokio::sync::Mutex::new(rx),
            }
        }

        /// Pre-load a snapshot to be returned by the next `snapshot()` call.
        pub fn push_snapshot(&self, s: SourceSnapshot) {
            self.snapshots.lock().unwrap().push(s);
        }

        /// Simulate an HCTL element-change event, waking any `next_trigger()`
        /// waiter.
        pub fn fire_trigger(&self) {
            let _ = self.trigger_tx.send(());
        }
    }

    #[async_trait]
    impl SourceObserver for MockSourceObserver {
        async fn snapshot(&self) -> Result<SourceSnapshot, PicorecdspError> {
            let mut queue = self.snapshots.lock().unwrap();
            queue.pop().ok_or_else(|| {
                PicorecdspError::SourceObserver("MockSourceObserver: no snapshot queued".into())
            })
        }

        async fn next_trigger(&mut self) -> Result<(), PicorecdspError> {
            self.trigger_rx
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| PicorecdspError::SourceObserver("trigger channel closed".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceState;
    use testing::MockSourceObserver;

    #[tokio::test]
    async fn mock_observer_returns_queued_snapshot() {
        let obs = MockSourceObserver::new();
        obs.push_snapshot(SourceSnapshot {
            state: SourceState::Active {
                sample_rate: 44_100,
            },
            sample_rate: Some(44_100),
            format: Some("S32_LE".into()),
            channels: Some(2),
            generation: 1,
        });
        let snap = obs.snapshot().await.unwrap();
        assert!(snap.is_active());
        assert_eq!(snap.sample_rate, Some(44_100));
    }

    #[tokio::test]
    async fn mock_observer_trigger_wakes_waiter() {
        let mut obs = MockSourceObserver::new();
        obs.fire_trigger();
        obs.next_trigger().await.unwrap();
    }
}
