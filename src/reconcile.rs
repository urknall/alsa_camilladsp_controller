//! Reconciliation loop (roadmap §10, §37).
//!
//! The reconciler is **not** a historical state machine.  On every trigger it:
//!
//! 1. Reads a fresh overall snapshot (source + DSP state + configs).
//! 2. Determines the desired state.
//! 3. Takes the minimal necessary action.
//! 4. Settles (short debounce or yield).
//! 5. Re-reads and verifies.
//!
//! Events carry no truth.  They only cause a fresh snapshot.
//!
//! # Five state truths (roadmap §8)
//!
//! | Truth | Source | Rust type |
//! |-------|--------|-----------|
//! | 1 — Source transport state | `snd-aloop` HCTL | [`SourceSnapshot`] |
//! | 2 — DSP process state | CamillaDSP WebSocket | [`DspState`] |
//! | 3 — Applied runtime config | `GetConfig`/`GetPreviousConfig` | [`ConfigDocument`] |
//! | 4 — Persistent config state | `GetConfigFilePath` | [`std::path::PathBuf`] |
//! | 5 — GUI draft state | Not observed by Rust | — |
//!
//! # Hard config invariants enforced here (roadmap §9)
//!
//! * Rust never writes user YAML (filters, mixer, pipeline, FIR, etc.).
//! * No `runtime.yml`.
//! * No shadow config file.
//! * `Save != Apply`: a saved-but-not-applied config change produces no reload.
//! * GUI Apply is never rolled back by Rust.
//! * Config switches are never reverted by Rust.
//! * `ConfigFilePath != RuntimeConfig` is a legitimate state, not an error.

use std::time::Duration;

use crate::{
    camilla::{
        control::DspState,
        {read_capture_transport_config, validate_transport_contract},
    },
    error::PicorecdspError,
    rate_sync::{DspSnapshot, RateSyncOutcome, SourceRateSynchronizer},
    source::SourceSnapshot,
};

use crate::camilla::CamillaControl;

// ── Reconcile configuration ───────────────────────────────────────────────────

/// Tuning parameters for the reconcile loop.  Defaults match the roadmap's
/// recommended values.
#[derive(Debug, Clone)]
pub struct ReconcileConfig {
    /// How long to wait after a state event before re-reading (debounce).
    /// Roadmap §11 suggests ~50 ms.
    pub debounce: Duration,

    /// How long to sleep between settle reads before considering a state
    /// "settled".
    pub settle_poll_interval: Duration,

    /// Maximum number of settle polls before giving up and scheduling a retry.
    pub max_settle_polls: u32,

    /// Interval for the slow safety reconcile (runs regardless of triggers).
    pub safety_reconcile_interval: Duration,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(50),
            settle_poll_interval: Duration::from_millis(100),
            max_settle_polls: 20, // up to 2 s
            safety_reconcile_interval: Duration::from_secs(30),
        }
    }
}

// ── Reconcile outcome (for testing / logging) ─────────────────────────────────

/// What action (if any) the reconciler took on a single pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    /// Source is inactive; waiting for `stop_on_inactive` to fire.
    WaitingForSourceStop,

    /// The transport contract is violated; managed mode is suspended.
    ManagedModeSuspended { reason: String },

    /// DSP is in a transitional state; reconcile deferred.
    DspTransitioning,

    /// DSP was running/paused; rate was already correct or was patched.
    RateSyncWhileRunning(RateSyncOutcome),

    /// DSP was settled inactive; rate was synced via `SetConfig`.
    RateSyncAfterInactive(RateSyncOutcome),

    /// DSP is in an error/stalled state; bounded retry scheduled.
    DspError { state: DspState },

    /// No action was needed on this pass.
    Idle,
}

// ── CamillaDsp snapshot helper ────────────────────────────────────────────────

/// Read a fresh [`DspSnapshot`] from `camilla`.
///
/// This is always a set of independent fresh reads — nothing is cached.
async fn read_dsp_snapshot(camilla: &dyn CamillaControl) -> Result<DspSnapshot, PicorecdspError> {
    let state = camilla.state().await?;
    let active_config = camilla.active_config().await?;
    let previous_config = camilla.previous_config().await?;
    let active_fingerprint = active_config.as_ref().map(|c| c.fingerprint());
    let previous_fingerprint = previous_config.as_ref().map(|c| c.fingerprint());
    Ok(DspSnapshot {
        state,
        active_config,
        previous_config,
        active_fingerprint,
        previous_fingerprint,
    })
}

// ── Core reconcile step ───────────────────────────────────────────────────────

/// Run one reconcile step.
///
/// `source` — fresh source snapshot (just read, never cached).
/// `dsp`    — fresh DSP snapshot (just read, never cached).
/// `rate_sync` — rate synchronizer to use for patches.
///
/// Returns the action taken, for logging and testing.
pub async fn reconcile_step(
    source: &SourceSnapshot,
    dsp: &DspSnapshot,
    rate_sync: &dyn SourceRateSynchronizer,
) -> Result<ReconcileAction, PicorecdspError> {
    // ── Step 1: source inactive ───────────────────────────────────────────
    if !source.is_active() {
        // Normal stop lifecycle (roadmap §12): Rust performs no stop of its own.
        // CamillaDSP's `stop_on_inactive` handles the capture release.
        // Rust gives CamillaDSP its grace phase and then checks real state.
        return Ok(ReconcileAction::WaitingForSourceStop);
    }

    // ── Step 2: transport contract check (roadmap §7) ─────────────────────
    // Only check if we have an authoritative config to validate against.
    if let Some(config) = dsp.authoritative_config() {
        let yaml = config
            .to_yaml()
            .map_err(|e| PicorecdspError::ConfigRead(e.to_string()))?;
        let transport = read_capture_transport_config(&yaml)
            .map_err(|e| PicorecdspError::ConfigRead(e.to_string()))?;
        match validate_transport_contract(&transport) {
            Ok(()) => {}
            Err(violations) => {
                let reason = violations
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                return Ok(ReconcileAction::ManagedModeSuspended { reason });
            }
        }
    }

    // ── Step 3: DSP transitional — wait ───────────────────────────────────
    if dsp.state.is_transitional() {
        return Ok(ReconcileAction::DspTransitioning);
    }

    // ── Step 4: DSP error / stalled — classify, bounded retry ─────────────
    if matches!(dsp.state, DspState::Stalled | DspState::Failed) {
        return Ok(ReconcileAction::DspError { state: dsp.state });
    }

    // ── Step 5: get the source rate ───────────────────────────────────────
    let source_rate = match source.sample_rate {
        Some(r) => r,
        None => {
            // Source is active per is_active() but has no rate; shouldn't happen
            // with a correct ALSA impl, but be defensive.
            return Ok(ReconcileAction::Idle);
        }
    };

    // ── Step 6: DSP running/paused or settled inactive — sync rate ─────────
    // roadmap §16 (running) / §17 (inactive).
    if dsp.state.is_active() || dsp.state.is_settled_inactive() {
        // Ensure we have an authoritative config before attempting rate sync.
        if !dsp.has_authoritative_config() {
            return Ok(ReconcileAction::Idle);
        }

        let outcome = rate_sync.ensure_source_rate(source_rate, dsp).await?;

        if dsp.state.is_active() {
            return Ok(ReconcileAction::RateSyncWhileRunning(outcome));
        } else {
            return Ok(ReconcileAction::RateSyncAfterInactive(outcome));
        }
    }

    Ok(ReconcileAction::Idle)
}

// ── Settled-state detection (roadmap §13) ─────────────────────────────────────

/// Wait for the DSP state to leave a transitional condition.
///
/// Polls at `cfg.settle_poll_interval` up to `cfg.max_settle_polls` times.
/// Returns the last observed [`DspSnapshot`] (settled or not — the caller must
/// check `state.is_transitional()`).
pub async fn wait_for_settle(
    camilla: &dyn CamillaControl,
    cfg: &ReconcileConfig,
) -> Result<DspSnapshot, PicorecdspError> {
    for _ in 0..cfg.max_settle_polls {
        let snap = read_dsp_snapshot(camilla).await?;
        if !snap.state.is_transitional() {
            return Ok(snap);
        }
        tokio::time::sleep(cfg.settle_poll_interval).await;
    }
    read_dsp_snapshot(camilla).await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        camilla::{
            config_document::ConfigDocument,
            control::{DspState, StopReason, Version},
        },
        error::PicorecdspError,
        rate_sync::{DspSnapshot, RateSyncOutcome, SourceRateSynchronizer},
        source::{SourceSnapshot, SourceState},
    };
    use async_trait::async_trait;
    use serde_json::Value;
    use std::path::PathBuf;

    // ── Test doubles ────────────────────────────────────────────────────────

    #[derive(Clone, Default)]
    struct FakeCamilla {
        state: DspState,
        active_config: Option<ConfigDocument>,
        previous_config: Option<ConfigDocument>,
    }

    impl Default for DspState {
        fn default() -> Self {
            DspState::Inactive
        }
    }

    #[async_trait]
    impl CamillaControl for FakeCamilla {
        async fn version(&self) -> Result<Version, PicorecdspError> {
            Ok(Version::new(4, 2, 0))
        }
        async fn state(&self) -> Result<DspState, PicorecdspError> {
            Ok(self.state)
        }
        async fn stop_reason(&self) -> Result<Option<StopReason>, PicorecdspError> {
            Ok(None)
        }
        async fn active_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
            Ok(self.active_config.clone())
        }
        async fn previous_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
            Ok(self.previous_config.clone())
        }
        async fn config_file_path(&self) -> Result<Option<PathBuf>, PicorecdspError> {
            Ok(None)
        }
        async fn set_config(&self, _: &ConfigDocument) -> Result<(), PicorecdspError> {
            Ok(())
        }
        async fn set_config_value(&self, _: &str, _: Value) -> Result<(), PicorecdspError> {
            Ok(())
        }
        async fn stop(&self) -> Result<(), PicorecdspError> {
            Ok(())
        }
    }

    struct FakeRateSync {
        outcome: RateSyncOutcome,
    }

    #[async_trait]
    impl SourceRateSynchronizer for FakeRateSync {
        async fn ensure_source_rate(
            &self,
            _: u32,
            _: &DspSnapshot,
        ) -> Result<RateSyncOutcome, PicorecdspError> {
            Ok(self.outcome.clone())
        }
    }

    fn compliant_config(rate: u32) -> ConfigDocument {
        let yaml = format!(
            "devices:\n  samplerate: {rate}\n  capture:\n    type: Alsa\n    device: \"hw:Loopback,0,0\"\n    channels: 2\n    format: S32_LE\n    stop_on_inactive: true\n"
        );
        ConfigDocument::from_yaml(&yaml).unwrap()
    }

    fn active_source(rate: u32) -> SourceSnapshot {
        SourceSnapshot {
            state: SourceState::Active { sample_rate: rate },
            sample_rate: Some(rate),
            format: Some("S32_LE".into()),
            channels: Some(2),
            generation: 1,
        }
    }

    fn inactive_source() -> SourceSnapshot {
        SourceSnapshot {
            state: SourceState::Inactive,
            sample_rate: None,
            format: None,
            channels: None,
            generation: 0,
        }
    }

    fn dsp_snap(state: DspState, rate: u32) -> DspSnapshot {
        let doc = compliant_config(rate);
        let fp = doc.fingerprint();
        DspSnapshot {
            state,
            active_config: Some(doc.clone()),
            previous_config: Some(doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
        }
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn inactive_source_waits_for_stop() {
        let source = inactive_source();
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert_eq!(action, ReconcileAction::WaitingForSourceStop);
    }

    #[tokio::test]
    async fn incompatible_transport_suspends_managed_mode() {
        let source = active_source(44_100);
        // Config with wrong device — transport contract violation.
        let bad_yaml = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,1,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
"#;
        let doc = ConfigDocument::from_yaml(bad_yaml).unwrap();
        let fp = doc.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(doc.clone()),
            previous_config: Some(doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(action, ReconcileAction::ManagedModeSuspended { .. }),
            "expected suspension, got {action:?}"
        );
    }

    #[tokio::test]
    async fn transitional_dsp_defers() {
        let source = active_source(44_100);
        let mut dsp = dsp_snap(DspState::Starting, 44_100);
        dsp.active_config = None; // no config during startup
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert_eq!(action, ReconcileAction::DspTransitioning);
    }

    #[tokio::test]
    async fn running_dsp_rate_sync_already_correct() {
        let source = active_source(44_100);
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert_eq!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        );
    }

    #[tokio::test]
    async fn running_dsp_rate_sync_patches() {
        let source = active_source(96_000);
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::PatchedWhileRunning {
                old_rate: Some(44_100),
                new_rate: 96_000,
            },
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::PatchedWhileRunning {
                new_rate: 96_000,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn inactive_dsp_rate_sync_uses_set_config() {
        let source = active_source(96_000);
        let dsp = dsp_snap(DspState::Inactive, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::SetConfigAfterInactive {
                old_rate: Some(44_100),
                new_rate: 96_000,
            },
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::RateSyncAfterInactive(RateSyncOutcome::SetConfigAfterInactive {
                new_rate: 96_000,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn stalled_dsp_reports_error() {
        let source = active_source(44_100);
        let dsp = dsp_snap(DspState::Stalled, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::DspError {
                state: DspState::Stalled
            }
        ));
    }

    /// Mandatory regression: gain +6 dB applied without save must survive
    /// source rate 44.1 → 96 → 48 kHz (roadmap §30, §47).
    ///
    /// This test checks the reconciler's decision path; the actual config
    /// preservation is tested in `config_patch.rs`.
    #[tokio::test]
    async fn rate_change_cycle_takes_correct_action_path() {
        // 44.1 kHz active, DSP running at 44.1.
        let source_441 = active_source(44_100);
        let dsp_running_441 = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let a = reconcile_step(&source_441, &dsp_running_441, &sync)
            .await
            .unwrap();
        assert!(matches!(a, ReconcileAction::RateSyncWhileRunning(_)));

        // Source stops → inactive source.
        let source_inactive = inactive_source();
        let dsp_inactive = dsp_snap(DspState::Inactive, 44_100);
        let a = reconcile_step(&source_inactive, &dsp_inactive, &sync)
            .await
            .unwrap();
        assert_eq!(a, ReconcileAction::WaitingForSourceStop);

        // New producer at 96 kHz → DSP inactive, source active at 96.
        let source_96 = active_source(96_000);
        let sync96 = FakeRateSync {
            outcome: RateSyncOutcome::SetConfigAfterInactive {
                old_rate: Some(44_100),
                new_rate: 96_000,
            },
        };
        let a = reconcile_step(&source_96, &dsp_inactive, &sync96)
            .await
            .unwrap();
        assert!(matches!(a, ReconcileAction::RateSyncAfterInactive(_)));

        // New producer at 48 kHz.
        let source_48 = active_source(48_000);
        let sync48 = FakeRateSync {
            outcome: RateSyncOutcome::SetConfigAfterInactive {
                old_rate: Some(96_000),
                new_rate: 48_000,
            },
        };
        let a = reconcile_step(&source_48, &dsp_inactive, &sync48)
            .await
            .unwrap();
        assert!(matches!(a, ReconcileAction::RateSyncAfterInactive(_)));
    }
}
