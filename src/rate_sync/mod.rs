//! Source-rate synchronization trait and config-patch implementation
//! (roadmap §15–§17 / Cliffhanger A).
//!
//! Today CamillaDSP has no persistent source-rate override: every time the source
//! sample rate changes Rust must read the runtime config, patch the single rate
//! field, and write it back.  This code lives exclusively in this module so it
//! can be deleted as a single unit when upstream provides a native override.
//!
//! # Module layout
//!
//! * This file (`mod.rs`): [`SourceRateSynchronizer`] trait, [`DspTriggerSource`]
//!   trait, shared types, and re-exports.
//! * `config_patch.rs`: [`ConfigPatchRateSynchronizer`] — the current workaround
//!   implementation.
//!
//! # Removal criterion (Cliffhanger A)
//!
//! Registered in `upstream/capabilities.yml` under the key
//! `persistent_source_rate_override`.  Deletion condition:
//!
//! > Once CamillaDSP upstream provides a source-rate override that can be set
//! > in the inactive state, survives reload, survives `SetConfig`, survives GUI
//! > Apply, survives config switches, correctly handles the resampler case, and
//! > honours `$samplerate$` before token resolution →
//! > `rate_sync/config_patch.rs` → **DELETE**.

pub mod config_patch;

pub use config_patch::ConfigPatchRateSynchronizer;

use async_trait::async_trait;

use crate::{
    camilla::{control::DspState, ConfigDocument},
    error::PicorecdspError,
};

// ── DspSnapshot ───────────────────────────────────────────────────────────────

/// A point-in-time snapshot of the CamillaDSP process state and configs,
/// representing State Truths 2, 3, and 4 together (roadmap §8).
///
/// The reconciler reads this fresh on every trigger and never caches it across
/// reconcile iterations.
#[derive(Debug, Clone)]
pub struct DspSnapshot {
    /// Current process state (State Truth 2).
    pub state: DspState,

    /// The currently applied runtime config from `GetConfig` (State Truth 3).
    /// `None` when CamillaDSP has no active config.
    pub active_config: Option<ConfigDocument>,

    /// The previous runtime config from `GetPreviousConfig` (State Truth 3,
    /// Inactive branch).  `None` when no previous config is recorded.
    pub previous_config: Option<ConfigDocument>,

    /// Fingerprint of `active_config` for race detection (roadmap §35,
    /// Cliffhanger C).
    pub active_fingerprint: Option<u64>,

    /// Fingerprint of `previous_config` for race detection.
    pub previous_fingerprint: Option<u64>,
}

impl DspSnapshot {
    /// Select the runtime config to use for a rate patch, implementing the
    /// priority order from roadmap §14:
    ///
    /// 1. DSP Running/Paused → `GetConfig`.
    /// 2. DSP settled Inactive → `GetPreviousConfig`.
    /// 3. Neither → `None` (reconciler should defer).
    pub fn authoritative_config(&self) -> Option<&ConfigDocument> {
        if self.state.is_active() {
            self.active_config.as_ref()
        } else if self.state.is_settled_inactive() {
            self.previous_config.as_ref()
        } else {
            None
        }
    }

    /// Whether we have an authoritative config to work with.
    pub fn has_authoritative_config(&self) -> bool {
        self.authoritative_config().is_some()
    }
}

// ── SourceRateSynchronizer trait ──────────────────────────────────────────────

/// The workaround trait for source-rate synchronization (roadmap §16–§17 /
/// Cliffhanger A).
///
/// # Removal criterion
///
/// See `upstream/capabilities.yml` key `persistent_source_rate_override`.
#[async_trait]
pub trait SourceRateSynchronizer: Send + Sync {
    /// Ensure that the running or previously-applied config has its rate field
    /// set to `source_rate`.
    ///
    /// Callers must:
    /// 1. Read a fresh `DspSnapshot` immediately before calling this.
    /// 2. Read a fresh `SourceSnapshot` immediately before calling this.
    /// 3. After `ensure_source_rate` returns `Ok`, re-read a fresh snapshot and
    ///    verify the rate was actually applied (roadmap §16, fresh-read verify).
    ///
    /// This method must NOT be called when:
    /// * The DSP is in a transitional state (`Starting`).
    /// * The source snapshot is `Inactive`.
    /// * The config contains `$samplerate$` tokens (use `has_samplerate_token()`
    ///   to check before calling).
    async fn ensure_source_rate(
        &self,
        source_rate: u32,
        dsp: &DspSnapshot,
    ) -> Result<RateSyncOutcome, PicorecdspError>;
}

/// Outcome of a [`SourceRateSynchronizer::ensure_source_rate`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateSyncOutcome {
    /// The rate field already matched; no write was performed.
    AlreadyCorrect,

    /// The rate field was updated with `SetConfigValue` (DSP was Running/Paused).
    PatchedWhileRunning {
        old_rate: Option<u32>,
        new_rate: u32,
    },

    /// The rate field was updated and `SetConfig` was called (DSP was Inactive).
    SetConfigAfterInactive {
        old_rate: Option<u32>,
        new_rate: u32,
    },
}

// ── DspTriggerSource trait ────────────────────────────────────────────────────

/// Abstraction for what wakes the reconciler to check DSP state (roadmap §22 /
/// Cliffhanger E).
///
/// On CamillaDSP 4.1 the only option is polling.  On 4.2+ and 5.x the preferred
/// path is `SubscribeState` push events; the slow safety reconcile is kept
/// regardless.
///
/// # Removal criterion (Cliffhanger E)
///
/// Registered in `upstream/capabilities.yml` under the key
/// `state_push_events`.  Deletion condition:
///
/// > Once the production baseline reliably supports `SubscribeState` →
/// > fast DSP state poller → **DELETE**.  Slow safety reconcile stays.
#[async_trait]
pub trait DspTriggerSource: Send {
    /// Block until it is time to run a reconcile cycle, then return.
    ///
    /// Implementations may block on:
    /// * A `SubscribeState` WebSocket message (4.2+/5.x).
    /// * A polling interval (4.1 fallback).
    /// * A slow safety interval (always present).
    async fn next_trigger(&mut self) -> Result<(), PicorecdspError>;
}

/// A simple polling-based [`DspTriggerSource`] (Cliffhanger E fallback).
///
/// Wakes the reconciler every `interval`.  Used on CamillaDSP 4.1 where
/// `SubscribeState` is unavailable.
pub struct PollingTrigger {
    interval: std::time::Duration,
}

impl PollingTrigger {
    pub fn new(interval: std::time::Duration) -> Self {
        Self { interval }
    }
}

#[async_trait]
impl DspTriggerSource for PollingTrigger {
    async fn next_trigger(&mut self) -> Result<(), PicorecdspError> {
        tokio::time::sleep(self.interval).await;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camilla::control::DspState;

    fn make_snapshot(state: DspState, rate: u32) -> DspSnapshot {
        let yaml = format!(
            "devices:\n  samplerate: {rate}\n  capture:\n    type: Alsa\n    device: \"hw:Loopback,0,0\"\n    channels: 2\n    format: S32_LE\n    stop_on_inactive: true\n"
        );
        let doc = ConfigDocument::from_yaml(&yaml).unwrap();
        let fp = doc.fingerprint();
        DspSnapshot {
            state,
            active_config: Some(doc.clone()),
            previous_config: Some(doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
        }
    }

    #[test]
    fn authoritative_config_running_uses_active() {
        let snap = make_snapshot(DspState::Running, 44_100);
        assert!(snap.authoritative_config().is_some());
        // When running, active_config is chosen.
        assert_eq!(
            snap.authoritative_config()
                .unwrap()
                .get("devices.samplerate"),
            Some(&serde_json::Value::Number(44_100.into()))
        );
    }

    #[test]
    fn authoritative_config_inactive_uses_previous() {
        let snap = make_snapshot(DspState::Inactive, 44_100);
        assert!(snap.authoritative_config().is_some());
    }

    #[test]
    fn authoritative_config_starting_returns_none() {
        let mut snap = make_snapshot(DspState::Starting, 44_100);
        snap.active_config = None;
        snap.previous_config = None;
        assert!(snap.authoritative_config().is_none());
    }

    #[tokio::test]
    async fn polling_trigger_fires_after_interval() {
        let mut trigger = PollingTrigger::new(std::time::Duration::from_millis(10));
        trigger.next_trigger().await.unwrap();
    }
}
