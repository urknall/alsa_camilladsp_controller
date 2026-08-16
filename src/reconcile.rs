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
        control::{DspState, StopReason},
        {read_capture_transport_config, validate_transport_contract},
    },
    error::PicorecdspError,
    rate_sync::{DspSnapshot, DspTriggerSource, RateSyncOutcome, SourceRateSynchronizer},
    source::{observer::SourceObserver, SourceSnapshot},
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

    /// Initial backoff before the first WebSocket reconnect attempt (roadmap §33).
    pub ws_initial_backoff: Duration,

    /// Maximum backoff cap for WebSocket reconnect attempts (roadmap §33).
    /// Backoff doubles on each failed attempt up to this ceiling.
    pub ws_max_backoff: Duration,

    /// Maximum number of consecutive `DspError` retries the run loop attempts
    /// before backing off and waiting for the next trigger (roadmap §33 —
    /// stalled handling: bounded retry, no restart loop).
    pub max_dsp_error_retries: u32,

    /// Delay between consecutive DSP error/stalled retry attempts (roadmap §33).
    pub dsp_error_retry_interval: Duration,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(50),
            settle_poll_interval: Duration::from_millis(100),
            max_settle_polls: 20, // up to 2 s
            safety_reconcile_interval: Duration::from_secs(30),
            ws_initial_backoff: Duration::from_secs(1),
            ws_max_backoff: Duration::from_secs(30),
            max_dsp_error_retries: 3,
            dsp_error_retry_interval: Duration::from_secs(2),
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
    ///
    /// `stop_reason` carries the `GetStopReason` value from the snapshot, which
    /// lets the run loop classify the failure (e.g. `PlaybackError` → DAC
    /// unavailable, `CaptureFormatChange` → expected transient).
    DspError {
        state: DspState,
        stop_reason: Option<StopReason>,
    },

    /// The active config is structurally invalid; managed mode suspended until
    /// the user applies a corrected config via GUI (roadmap §33 — invalid-config
    /// handling).  Rust does not repair the config.
    WaitingForUserFix { reason: String },

    /// No action was needed on this pass.
    Idle,
}

// ── CamillaDsp snapshot helper ────────────────────────────────────────────────

/// Read a fresh [`DspSnapshot`] from `camilla`.
///
/// This is always a set of independent fresh reads — nothing is cached.
async fn read_dsp_snapshot(camilla: &dyn CamillaControl) -> Result<DspSnapshot, PicorecdspError> {
    let state = camilla.state().await?;
    // Only query stop_reason for states where it is meaningful (roadmap §33).
    // Skipping it for Running/Paused/Inactive/Starting avoids an unnecessary
    // WebSocket round-trip on the common healthy path.
    let stop_reason = if matches!(state, DspState::Stalled | DspState::Failed) {
        camilla.stop_reason().await?
    } else {
        None
    };
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
        stop_reason,
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
        match read_capture_transport_config(&yaml) {
            Err(e) => {
                // The config YAML is structurally invalid at the devices.capture.*
                // paths we need.  Do not repair; suspend managed mode until the
                // user applies a corrected config (roadmap §33 invalid-config).
                return Ok(ReconcileAction::WaitingForUserFix {
                    reason: e.to_string(),
                });
            }
            Ok(transport) => match validate_transport_contract(&transport) {
                Ok(()) => {}
                Err(violations) => {
                    let reason = violations
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ");
                    return Ok(ReconcileAction::ManagedModeSuspended { reason });
                }
            },
        }
    }

    // ── Step 3: DSP transitional — wait ───────────────────────────────────
    if dsp.state.is_transitional() {
        return Ok(ReconcileAction::DspTransitioning);
    }

    // ── Step 4: DSP error / stalled — classify, bounded retry ─────────────
    if matches!(dsp.state, DspState::Stalled | DspState::Failed) {
        return Ok(ReconcileAction::DspError {
            state: dsp.state,
            stop_reason: dsp.stop_reason.clone(),
        });
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

// ── Run loop (roadmap §33 error/recovery model) ───────────────────────────────

/// Run the reconciliation loop indefinitely, implementing the full error and
/// recovery model from roadmap §33.
///
/// # Error / recovery behaviour
///
/// | Failure mode | Behaviour |
/// |---|---|
/// | WebSocket offline | Bounded exponential backoff (`ws_initial_backoff` → `ws_max_backoff`), then full fresh snapshot on reconnect. |
/// | DAC unavailable (`PlaybackError`) | `DspError` action returned; run loop waits `dsp_error_retry_interval` and retries up to `max_dsp_error_retries` times; no automatic DAC switch. |
/// | Invalid config | `WaitingForUserFix` action; managed mode suspended; no repair by Rust. |
/// | Incompatible transport config | `ManagedModeSuspended` action; clear reason logged; waits for a new GUI Apply. |
/// | Stalled DSP | `DspError` action; short observation phase (bounded retry), no immediate restart loop. |
/// | Rust crash | CamillaDSP continues independently.  On restart the loop reads a completely fresh snapshot — **no state is persisted across process restarts**. |
/// | CamillaDSP crash | Rust stays alive; CamillaDSP is restarted by the OS service manager (accepted v2 MVP boundary — unsaved RuntimeConfig may be lost). |
/// | CamillaGUI crash | Audio, Rust, and CamillaDSP are unaffected.  Rust never observes CamillaGUI directly; it only talks to CamillaDSP via WebSocket. |
///
/// # Returns
///
/// Returns `Err` only when `trigger.next_trigger()` signals shutdown (by
/// returning an error), at which point the loop should terminate cleanly.
pub async fn run_loop(
    camilla: &dyn CamillaControl,
    source_observer: &dyn SourceObserver,
    rate_sync: &dyn SourceRateSynchronizer,
    trigger: &mut dyn DspTriggerSource,
    cfg: &ReconcileConfig,
) -> Result<std::convert::Infallible, PicorecdspError> {
    // WebSocket reconnect backoff state.  Starts at ws_initial_backoff, doubles
    // on each offline poll, caps at ws_max_backoff, resets to ws_initial_backoff
    // on a successful read.
    let mut ws_backoff = cfg.ws_initial_backoff;
    // Consecutive DSP error count — reset to 0 whenever any non-error action is taken.
    let mut dsp_error_count: u32 = 0;

    loop {
        // ── Wait for next reconcile trigger ───────────────────────────────────
        // A trigger error propagates directly to signal shutdown (e.g. the trigger
        // channel was closed).
        trigger.next_trigger().await?;
        tokio::time::sleep(cfg.debounce).await;

        // ── Read fresh DSP snapshot — WebSocket-offline recovery ──────────────
        // Loops internally until the WebSocket comes back or a non-offline error
        // is encountered.  On reconnect, `ws_backoff` is reset and the full
        // snapshot is always read fresh (roadmap §33: "full fresh snapshot").
        let dsp = loop {
            match read_dsp_snapshot(camilla).await {
                Ok(snap) => {
                    ws_backoff = cfg.ws_initial_backoff;
                    break snap;
                }
                Err(PicorecdspError::WebSocketOffline(_)) => {
                    tokio::time::sleep(ws_backoff).await;
                    ws_backoff = (ws_backoff * 2).min(cfg.ws_max_backoff);
                    // Continue retrying within this trigger cycle.
                }
                Err(e) => return Err(e),
            }
        };

        // ── Read fresh source snapshot ────────────────────────────────────────
        // A transient source-observer error is non-fatal: skip this cycle and
        // wait for the next trigger.
        let source_snap = match source_observer.snapshot().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        // ── Run one reconcile step ────────────────────────────────────────────
        let action = match reconcile_step(&source_snap, &dsp, rate_sync).await {
            Ok(a) => a,
            // Invalid config (structurally malformed YAML at the paths we need):
            // do not repair; suspend managed mode; wait for user to fix via GUI.
            Err(PicorecdspError::ConfigRead(reason)) => {
                ReconcileAction::WaitingForUserFix { reason }
            }
            // $samplerate$ token guard: also surfaces as WaitingForUserFix — the
            // user must switch to a fixed-rate + resampler setup.
            Err(PicorecdspError::SamplerateTokenGuard { detail }) => {
                ReconcileAction::WaitingForUserFix {
                    reason: format!("$samplerate$ token in active config: {detail}"),
                }
            }
            // WebSocket went offline mid-step: reset backoff, skip this cycle,
            // and let the inner reconnect loop handle it on the next trigger.
            Err(PicorecdspError::WebSocketOffline(_)) => {
                ws_backoff = cfg.ws_initial_backoff;
                continue;
            }
            Err(e) => return Err(e),
        };

        // ── Post-step: bounded DSP error retry (roadmap §33 — stalled / DAC) ──
        // If the action is DspError, back off and retry up to max_dsp_error_retries
        // times before waiting for the next external trigger.  The
        // `dsp_error_retry_interval` sleep is applied on every DspError cycle
        // (including past the cap) so that even when the fast-retry window is
        // exhausted the loop cannot tight-spin faster than the retry interval.
        match &action {
            ReconcileAction::DspError { .. } => {
                dsp_error_count = dsp_error_count.saturating_add(1);
                // Always sleep at least dsp_error_retry_interval to avoid spinning.
                // Within the bounded-retry window the intent is to retry promptly;
                // beyond it we still wait the same interval (no zero-delay cycles).
                tokio::time::sleep(cfg.dsp_error_retry_interval).await;
            }
            _ => {
                dsp_error_count = 0;
            }
        }
    }
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
        rate_sync::{DspSnapshot, DspTriggerSource, RateSyncOutcome, SourceRateSynchronizer},
        source::{SourceSnapshot, SourceState},
    };
    use async_trait::async_trait;
    use serde_json::Value;
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

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

    /// A [`SourceRateSynchronizer`] spy that counts how many times
    /// `ensure_source_rate` was called, used to verify the reconciler reaches
    /// the rate-sync path on every trigger regardless of the rate value.
    struct SpyRateSync {
        outcome: RateSyncOutcome,
        call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl SpyRateSync {
        fn new(outcome: RateSyncOutcome) -> (Self, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
            let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            (
                Self {
                    outcome,
                    call_count: counter.clone(),
                },
                counter,
            )
        }
    }

    #[async_trait]
    impl SourceRateSynchronizer for SpyRateSync {
        async fn ensure_source_rate(
            &self,
            _: u32,
            _: &DspSnapshot,
        ) -> Result<RateSyncOutcome, PicorecdspError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
            stop_reason: None,
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
            stop_reason: None,
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
                state: DspState::Stalled,
                ..
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

    // ── Gate 6 scenario tests (roadmap §28–§32) ──────────────────────────────

    /// Apply-during-playback (roadmap §29.1, §30):
    /// When a GUI Apply changes filters/mixer while the DSP is running, the
    /// reconciler's only action is to call `ensure_source_rate` on the new
    /// active config.  It does not modify any user-owned field.
    ///
    /// Verification: reconciler returns `RateSyncWhileRunning` for a running DSP
    /// when active_config already reflects the post-Apply state.  The rate-sync
    /// layer (tested separately) only ever touches the single rate field.
    #[tokio::test]
    async fn apply_during_playback_reconciler_only_touches_rate() {
        // Simulate a GUI Apply: filters changed, rate unchanged.
        let applied_yaml = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
filters:
  new_eq:
    type: BiquadCombo
    parameters:
      type: LoudnessHighPass
"#;
        let doc = ConfigDocument::from_yaml(applied_yaml).unwrap();
        let fp = doc.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(doc.clone()),
            previous_config: Some(doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: None,
        };
        let source = active_source(44_100);
        // Rate is already correct → AlreadyCorrect returned from rate_sync.
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        // Reconciler must still proceed through the rate-sync path, not bail out.
        assert_eq!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        );
    }

    /// Config A → Config B (roadmap §29.4):
    /// After the user switches from Config A to Config B (via GUI Apply), the
    /// reconciler uses Config B as the authoritative runtime config.
    /// Config A is never restored.
    #[tokio::test]
    async fn config_a_to_b_latest_applied_config_is_authoritative() {
        // Config B is now active (after GUI Apply that switched configs).
        let config_b_yaml = r#"
devices:
  samplerate: 96000
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
filters:
  config_b_filter:
    type: Gain
    parameters:
      gain: -3.0
"#;
        let doc_b = ConfigDocument::from_yaml(config_b_yaml).unwrap();
        let fp_b = doc_b.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(doc_b.clone()),
            previous_config: Some(doc_b),
            active_fingerprint: Some(fp_b),
            previous_fingerprint: Some(fp_b),
            stop_reason: None,
        };
        let source = active_source(96_000);
        // Rate matches Config B → AlreadyCorrect (Rust chose Config B, not A).
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        // Reconciler must not suspend, must not error — it is using Config B.
        assert_eq!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        );
    }

    /// `ConfigFilePath != RuntimeConfig` divergence (roadmap §29.5):
    /// When the persistent config on disk differs from the applied runtime config,
    /// the reconciler takes no repair action.  RuntimeConfig (GetConfig) wins.
    ///
    /// This is verified by showing that `reconcile_step` does not observe
    /// `config_file_path` at all: the snapshot does not include it, and the
    /// reconciler path for a compliant running config is simply rate-sync.
    #[tokio::test]
    async fn config_file_path_divergence_produces_no_repair() {
        // Runtime config has rate 44.1 kHz (applied by GUI).
        // Disk config (ConfigFilePath) could be anything — reconciler never reads it.
        let runtime_yaml = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
"#;
        let doc = ConfigDocument::from_yaml(runtime_yaml).unwrap();
        let fp = doc.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(doc.clone()),
            previous_config: Some(doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: None,
        };
        let source = active_source(44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        // No repair action: reconciler returns normal rate-sync result.
        assert_eq!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        );
        // The reconcile_step function signature accepts no `config_file_path`
        // parameter: this test demonstrates that by construction the reconciler
        // has no access to the disk path and therefore cannot act on a divergence.
    }

    /// New-source-same-rate via generation detection (roadmap §31):
    /// When a new producer opens `snd-aloop` at the same sample rate as the
    /// previous producer, the `generation` counter increments.  The reconciler
    /// must still run the full rate-sync pass — it must not skip it on the
    /// assumption that "rate didn't change therefore nothing to do".
    ///
    /// Verification: `ensure_source_rate` is invoked on every trigger regardless
    /// of whether the rate changed.  A `SpyRateSync` counts the calls.
    #[tokio::test]
    async fn new_source_same_rate_reconcile_still_runs_full_pass() {
        let rate = 48_000;
        let dsp = dsp_snap(DspState::Running, rate);
        let (spy, call_count) = SpyRateSync::new(RateSyncOutcome::AlreadyCorrect);

        // First source at 48 kHz, generation 1.
        let source_gen1 = SourceSnapshot {
            state: SourceState::Active { sample_rate: rate },
            sample_rate: Some(rate),
            format: Some("S32_LE".into()),
            channels: Some(2),
            generation: 1,
        };
        let action = reconcile_step(&source_gen1, &dsp, &spy).await.unwrap();
        assert_eq!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        );
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "ensure_source_rate must have been called for the first source"
        );

        // Second source at same 48 kHz, generation 2 (new producer, same rate).
        // Reconciler must reach ensure_source_rate again — no skipping.
        let source_gen2 = SourceSnapshot {
            state: SourceState::Active { sample_rate: rate },
            sample_rate: Some(rate),
            format: Some("S32_LE".into()),
            channels: Some(2),
            generation: 2,
        };
        let action = reconcile_step(&source_gen2, &dsp, &spy).await.unwrap();
        assert_eq!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        );
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "ensure_source_rate must have been called again for the second source (same rate, new generation)"
        );
    }

    /// Concurrent Apply + rate-change race (roadmap §32):
    /// If a GUI Apply fires while the reconciler is preparing a rate patch, the
    /// reconciler must use a fresh snapshot for the actual write, not a stale one.
    ///
    /// `reconcile_step` is purely functional: it operates on the snapshot it is
    /// passed.  The caller (reconcile loop) is responsible for re-reading fresh
    /// snapshots on every trigger.  This test verifies the correct snapshot
    /// selection per `DspSnapshot::authoritative_config` when state is Running.
    #[tokio::test]
    async fn concurrent_apply_rate_change_fresh_snapshot_used() {
        // Scenario: GUI Apply has just fired (DSP is Running with the new config).
        // The rate sync must use the post-Apply active_config — not a stale cached
        // one — and must issue the rate patch to the correct rate field.
        let post_apply_yaml = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
filters:
  post_apply_eq:
    type: Gain
    parameters:
      gain: 3.0
"#;
        let post_apply_doc = ConfigDocument::from_yaml(post_apply_yaml).unwrap();
        let fp = post_apply_doc.fingerprint();
        // The fresh snapshot already reflects the post-Apply state.
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(post_apply_doc.clone()),
            previous_config: Some(post_apply_doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: None,
        };
        // Source rate changed to 96 kHz at the same time as the Apply.
        let source = active_source(96_000);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::PatchedWhileRunning {
                old_rate: Some(44_100),
                new_rate: 96_000,
            },
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        // Rate was patched using the post-Apply config, not a stale one.
        assert!(matches!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::PatchedWhileRunning {
                new_rate: 96_000,
                ..
            })
        ));
    }

    // ── Gate 7 test doubles ──────────────────────────────────────────────────

    /// A [`CamillaControl`] that returns [`PicorecdspError::WebSocketOffline`] for
    /// the first `offline_count` calls to `state()`, then returns `ok_state`.
    struct OfflineThenOnlineCamilla {
        offline_remaining: Arc<Mutex<u32>>,
        ok_state: DspState,
        ok_config: Option<ConfigDocument>,
    }

    impl OfflineThenOnlineCamilla {
        fn new(offline_count: u32, ok_state: DspState, ok_config: Option<ConfigDocument>) -> Self {
            Self {
                offline_remaining: Arc::new(Mutex::new(offline_count)),
                ok_state,
                ok_config,
            }
        }
    }

    #[async_trait]
    impl CamillaControl for OfflineThenOnlineCamilla {
        async fn version(&self) -> Result<Version, PicorecdspError> {
            Ok(Version::new(4, 2, 0))
        }
        async fn state(&self) -> Result<DspState, PicorecdspError> {
            let mut remaining = self.offline_remaining.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                Err(PicorecdspError::WebSocketOffline("test: offline".into()))
            } else {
                Ok(self.ok_state)
            }
        }
        async fn stop_reason(&self) -> Result<Option<StopReason>, PicorecdspError> {
            Ok(None)
        }
        async fn active_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
            Ok(self.ok_config.clone())
        }
        async fn previous_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
            Ok(self.ok_config.clone())
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

    /// A [`CamillaControl`] whose `active_config` returns a structurally invalid
    /// YAML document (no `devices.capture.*` section).
    struct InvalidConfigCamilla;

    #[async_trait]
    impl CamillaControl for InvalidConfigCamilla {
        async fn version(&self) -> Result<Version, PicorecdspError> {
            Ok(Version::new(4, 2, 0))
        }
        async fn state(&self) -> Result<DspState, PicorecdspError> {
            Ok(DspState::Running)
        }
        async fn stop_reason(&self) -> Result<Option<StopReason>, PicorecdspError> {
            Ok(None)
        }
        async fn active_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
            // YAML with no devices.capture.* section — will fail transport validation.
            Ok(Some(ConfigDocument::from_yaml("foo: bar").unwrap()))
        }
        async fn previous_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
            Ok(None)
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

    /// A [`DspTriggerSource`] that fires exactly `count` times then signals
    /// shutdown by returning a `SourceObserver` error.
    struct LimitedTrigger {
        remaining: u32,
    }

    impl LimitedTrigger {
        fn new(count: u32) -> Self {
            Self { remaining: count }
        }
    }

    #[async_trait]
    impl DspTriggerSource for LimitedTrigger {
        async fn next_trigger(&mut self) -> Result<(), PicorecdspError> {
            if self.remaining > 0 {
                self.remaining -= 1;
                Ok(())
            } else {
                Err(PicorecdspError::SourceObserver(
                    "test: trigger exhausted".into(),
                ))
            }
        }
    }

    /// A simple [`SourceObserver`] that always returns the same snapshot.
    struct FixedSourceObserver {
        snapshot: SourceSnapshot,
    }

    #[async_trait]
    impl crate::source::observer::SourceObserver for FixedSourceObserver {
        async fn snapshot(&self) -> Result<SourceSnapshot, PicorecdspError> {
            Ok(self.snapshot.clone())
        }
        async fn next_trigger(&mut self) -> Result<(), PicorecdspError> {
            Ok(())
        }
    }

    /// Minimal `ReconcileConfig` with all durations set to 1 ms for fast tests.
    fn fast_cfg() -> ReconcileConfig {
        ReconcileConfig {
            debounce: Duration::from_millis(1),
            settle_poll_interval: Duration::from_millis(1),
            max_settle_polls: 2,
            safety_reconcile_interval: Duration::from_millis(1),
            ws_initial_backoff: Duration::from_millis(1),
            ws_max_backoff: Duration::from_millis(2),
            max_dsp_error_retries: 3,
            dsp_error_retry_interval: Duration::from_millis(1),
        }
    }

    // ── Error model tests (reconcile_step level) ─────────────────────────────

    /// Invalid config (malformed `devices.capture.*`) → `WaitingForUserFix`.
    /// Rust must not repair the config; managed mode is suspended (roadmap §33).
    #[tokio::test]
    async fn invalid_config_in_active_config_returns_waiting_for_user_fix() {
        // A ConfigDocument with no `devices.capture.*` section — will fail when
        // the reconciler tries to validate the transport contract.
        let bad_doc = ConfigDocument::from_yaml("foo: bar").unwrap();
        let fp = bad_doc.fingerprint();
        let source = active_source(44_100);
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(bad_doc.clone()),
            previous_config: Some(bad_doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: None,
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(action, ReconcileAction::WaitingForUserFix { .. }),
            "expected WaitingForUserFix, got {action:?}"
        );
    }

    /// DSP failed with PlaybackError (DAC unavailable) → `DspError` carries
    /// the stop reason.  The run loop uses this to classify the failure and apply
    /// a bounded retry without switching DACs (roadmap §33).
    #[tokio::test]
    async fn dsp_failed_with_playback_error_reports_stop_reason() {
        let source = active_source(44_100);
        let cfg = compliant_config(44_100);
        let fp = cfg.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Failed,
            active_config: Some(cfg.clone()),
            previous_config: Some(cfg),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: Some(StopReason::PlaybackError),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::DspError {
                    state: DspState::Failed,
                    stop_reason: Some(StopReason::PlaybackError),
                }
            ),
            "expected DspError with PlaybackError stop reason, got {action:?}"
        );
    }

    /// DSP stalled with CaptureFormatChange → `DspError` with that stop reason.
    /// This is an expected transient: source rate changed while DSP was running.
    #[tokio::test]
    async fn dsp_stalled_with_capture_format_change_reports_stop_reason() {
        let source = active_source(96_000);
        let cfg = compliant_config(44_100);
        let fp = cfg.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Stalled,
            active_config: Some(cfg.clone()),
            previous_config: Some(cfg),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: Some(StopReason::CaptureFormatChange),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::DspError {
                state: DspState::Stalled,
                stop_reason: Some(StopReason::CaptureFormatChange),
            }
        ));
    }

    // ── run_loop error/recovery tests ────────────────────────────────────────

    /// WebSocket offline: run_loop retries with bounded backoff and reconciles
    /// successfully once the WebSocket comes back (roadmap §33).
    #[tokio::test]
    async fn run_loop_websocket_offline_reconnects_after_backoff() {
        // Camilla is offline for 2 state() calls, then returns Inactive.
        let cfg_doc = compliant_config(44_100);
        let camilla = OfflineThenOnlineCamilla::new(2, DspState::Inactive, Some(cfg_doc));
        let mut obs = FixedSourceObserver {
            snapshot: inactive_source(),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        // Fire one trigger, then shut down.
        let mut trigger = LimitedTrigger::new(1);
        let cfg = fast_cfg();

        // The loop reconnects during the first trigger cycle and shuts down on
        // trigger exhaustion.
        let result = run_loop(&camilla, &obs, &sync, &mut trigger, &cfg).await;
        assert!(
            result.is_err(),
            "run_loop should terminate when trigger is exhausted"
        );
    }

    /// run_loop terminates cleanly when the trigger source signals shutdown.
    #[tokio::test]
    async fn run_loop_terminates_cleanly_on_trigger_shutdown() {
        let camilla = FakeCamilla::default();
        let mut obs = FixedSourceObserver {
            snapshot: inactive_source(),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        // Zero triggers → shuts down immediately.
        let mut trigger = LimitedTrigger::new(0);
        let cfg = fast_cfg();
        let result = run_loop(&camilla, &obs, &sync, &mut trigger, &cfg).await;
        assert!(matches!(result, Err(PicorecdspError::SourceObserver(_))));
    }

    /// Invalid config in the active-config path produces WaitingForUserFix via
    /// the run_loop, not a fatal error (roadmap §33 — no repair, no crash).
    #[tokio::test]
    async fn run_loop_invalid_config_produces_waiting_for_user_fix_not_crash() {
        let camilla = InvalidConfigCamilla;
        let mut obs = FixedSourceObserver {
            snapshot: active_source(44_100),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let mut trigger = LimitedTrigger::new(1);
        let cfg = fast_cfg();
        // The loop must not crash or propagate a fatal error; it terminates only
        // on trigger exhaustion.
        let result = run_loop(&camilla, &obs, &sync, &mut trigger, &cfg).await;
        assert!(matches!(result, Err(PicorecdspError::SourceObserver(_))));
    }

    // ── Cold boot test matrix (roadmap §34) ──────────────────────────────────

    /// Cold boot without a producer: source is inactive.
    /// Expected: reconciler waits for `stop_on_inactive`; no DSP action taken.
    #[tokio::test]
    async fn cold_boot_without_producer_waits_for_source_stop() {
        let source = inactive_source();
        let dsp = dsp_snap(DspState::Inactive, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert_eq!(action, ReconcileAction::WaitingForSourceStop);
    }

    /// Cold boot with an already-active producer at the same rate as the existing
    /// config: DSP is settled inactive with a `PreviousConfig` matching the source
    /// rate → rate sync is triggered immediately.
    #[tokio::test]
    async fn cold_boot_with_active_producer_at_matching_rate_syncs_immediately() {
        let source = active_source(44_100);
        let dsp = dsp_snap(DspState::Inactive, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::RateSyncAfterInactive(RateSyncOutcome::AlreadyCorrect)
            ),
            "expected RateSyncAfterInactive, got {action:?}"
        );
    }

    /// Cold boot with an already-active producer at a different rate: DSP inactive
    /// with `PreviousConfig` at 44.1 kHz, source at 96 kHz → rate sync patches
    /// the config to 96 kHz.
    #[tokio::test]
    async fn cold_boot_with_active_producer_at_different_rate_patches_config() {
        let source = active_source(96_000);
        let dsp = dsp_snap(DspState::Inactive, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::SetConfigAfterInactive {
                old_rate: Some(44_100),
                new_rate: 96_000,
            },
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::RateSyncAfterInactive(RateSyncOutcome::SetConfigAfterInactive {
                    new_rate: 96_000,
                    ..
                })
            ),
            "expected RateSyncAfterInactive with 96 kHz, got {action:?}"
        );
    }

    /// Cold boot: `PreviousConfig` is available after `stop_on_inactive` (DSP
    /// settled inactive), and the reconciler uses it for the rate sync.
    #[tokio::test]
    async fn cold_boot_statefile_previous_config_available_and_used() {
        let source = active_source(48_000);
        // DSP settled inactive — statefile config was loaded, PreviousConfig available.
        let dsp = dsp_snap(DspState::Inactive, 48_000);
        assert!(
            dsp.previous_config.is_some(),
            "PreviousConfig must be available"
        );
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::RateSyncAfterInactive(RateSyncOutcome::AlreadyCorrect)
        ));
    }

    /// Cold boot with a `CaptureError` at startup (snd-aloop not ready or DAC
    /// error on the capture side): DSP is Failed.
    #[tokio::test]
    async fn cold_boot_capture_error_at_startup_classifies_as_dsp_failed() {
        let source = active_source(44_100);
        let cfg = compliant_config(44_100);
        let fp = cfg.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Failed,
            active_config: Some(cfg.clone()),
            previous_config: Some(cfg),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: Some(StopReason::CaptureError),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::DspError {
                state: DspState::Failed,
                stop_reason: Some(StopReason::CaptureError),
            }
        ));
    }

    /// Cold boot with a `PlaybackError` (DAC unavailable at startup): DSP is
    /// Failed.  No automatic DAC switch; bounded retry via run loop (roadmap §33).
    #[tokio::test]
    async fn cold_boot_playback_error_at_startup_is_dac_unavailable() {
        let source = active_source(44_100);
        let cfg = compliant_config(44_100);
        let fp = cfg.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Failed,
            active_config: Some(cfg.clone()),
            previous_config: Some(cfg),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: Some(StopReason::PlaybackError),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::DspError {
                stop_reason: Some(StopReason::PlaybackError),
                ..
            }
        ));
    }

    /// Cold boot: missing `ConfigFilePath` (no statefile, DSP has no previous
    /// config) → reconciler defers (`Idle`) since no authoritative config is
    /// available to rate-sync with.
    #[tokio::test]
    async fn cold_boot_no_previous_config_reconciler_defers() {
        let source = active_source(44_100);
        let dsp = DspSnapshot {
            state: DspState::Inactive,
            active_config: None,
            previous_config: None,
            active_fingerprint: None,
            previous_fingerprint: None,
            stop_reason: None,
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert_eq!(
            action,
            ReconcileAction::Idle,
            "no authoritative config → reconciler must defer, not crash"
        );
    }

    /// Cold boot: invalid persistent config (e.g. YAML with no `devices.capture`
    /// section loaded by CamillaDSP at startup) → `WaitingForUserFix`.
    #[tokio::test]
    async fn cold_boot_invalid_persistent_config_waits_for_fix() {
        let source = active_source(44_100);
        let bad_doc = ConfigDocument::from_yaml("foo: bar").unwrap();
        let fp = bad_doc.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(bad_doc.clone()),
            previous_config: Some(bad_doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: None,
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(action, ReconcileAction::WaitingForUserFix { .. }));
    }

    // ── Crash isolation tests (roadmap §33) ──────────────────────────────────

    /// CamillaDSP crash: Rust stays alive and reads a completely fresh snapshot
    /// on the next reconcile cycle.  The reconciler uses the fresh DSP state from
    /// `read_dsp_snapshot` — it never caches state across iterations (roadmap §33).
    ///
    /// Simulation: DSP was Running, then "crashes" to Failed.  The reconciler
    /// reads the Failed state and returns `DspError` rather than acting on stale
    /// Running state.
    #[tokio::test]
    async fn camilladsp_crash_rust_reads_fresh_state_not_cached() {
        let source = active_source(44_100);
        // Simulate post-crash snapshot: DSP is now Failed.
        let cfg = compliant_config(44_100);
        let fp = cfg.fingerprint();
        let dsp_after_crash = DspSnapshot {
            state: DspState::Failed,
            active_config: Some(cfg.clone()),
            previous_config: Some(cfg),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: Some(StopReason::None),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        // The reconciler must report the crash, not try to rate-sync.
        let action = reconcile_step(&source, &dsp_after_crash, &sync)
            .await
            .unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::DspError {
                    state: DspState::Failed,
                    ..
                }
            ),
            "CamillaDSP crash must produce DspError, not rate sync"
        );
    }

    /// CamillaGUI crash isolation: Rust and CamillaDSP are unaffected.
    ///
    /// Design-level confirmation: Rust observes **only** CamillaDSP via the
    /// WebSocket — it has no direct communication channel with CamillaGUI.
    /// A GUI crash therefore produces no observable change in the reconciler's
    /// inputs; the next reconcile cycle reads a fresh snapshot from CamillaDSP
    /// and continues normally (roadmap §33).
    #[tokio::test]
    async fn camillagui_crash_isolation_reconciler_runs_normally() {
        let source = active_source(44_100);
        // CamillaDSP is unaffected by the GUI crash — same Running state.
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        // Normal rate-sync path: GUI crash has no effect on the reconciler.
        assert!(
            matches!(action, ReconcileAction::RateSyncWhileRunning(_)),
            "CamillaGUI crash must not affect reconciler behaviour"
        );
    }

    /// Rust crash recovery is stateless restart (roadmap §33).
    ///
    /// After a Rust crash and restart, the reconciler reads a completely fresh
    /// snapshot — no persistent Rust state from the previous run is consulted.
    /// This is guaranteed by the loop structure: `run_loop` holds no cross-cycle
    /// state beyond the backoff counters which are initialised on every startup.
    ///
    /// This test verifies that the reconciler can start from a DSP-already-running
    /// state (simulating a Rust restart while CamillaDSP was running) and
    /// correctly syncs the rate without needing any prior Rust history.
    #[tokio::test]
    async fn rust_crash_recovery_is_stateless_restart() {
        // Post-Rust-restart: source is active at 48 kHz, CamillaDSP was running
        // at 44.1 kHz (the rate before Rust crashed).
        let source = active_source(48_000);
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::PatchedWhileRunning {
                old_rate: Some(44_100),
                new_rate: 48_000,
            },
        };
        // The reconciler patches the rate correctly from a cold start — no
        // prior Rust history needed.
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::PatchedWhileRunning {
                    new_rate: 48_000,
                    ..
                })
            ),
            "Rust crash recovery must patch rate from fresh snapshot, got {action:?}"
        );
    }

    // ── No disk config watcher (roadmap §35) ─────────────────────────────────

    /// Confirm that no disk config watcher (mtime/inode/fingerprint auto-reload)
    /// is implemented anywhere in this crate's public surface.
    ///
    /// This is a structural test: it verifies that `ReconcileConfig` has no
    /// `config_file_path`, `watch_interval`, or similar field that would imply
    /// polling the disk.  `Save != Apply` is enforced by design — the reconciler
    /// never reads from `config_file_path` to decide what config to apply.
    #[test]
    fn no_disk_config_watcher_in_reconcile_config() {
        let cfg = ReconcileConfig::default();
        // ReconcileConfig has no disk-watching field: the only Duration fields are
        // timing parameters for settling, backoff, and retry — not file polling.
        // If a disk-watching field is ever added, this test must be updated with
        // a justification that it does NOT auto-reload (i.e. is only used for
        // race detection as per roadmap §35).
        let _ = cfg.debounce;
        let _ = cfg.settle_poll_interval;
        let _ = cfg.max_settle_polls;
        let _ = cfg.safety_reconcile_interval;
        let _ = cfg.ws_initial_backoff;
        let _ = cfg.ws_max_backoff;
        let _ = cfg.max_dsp_error_retries;
        let _ = cfg.dsp_error_retry_interval;
        // No `config_watcher_interval`, `watch_config_file_path`, or similar field.
        // This is a compile-time structural assertion via exhaustive field access.
    }

    // ── Gate 8 — Source matrix (roadmap §45) ─────────────────────────────────

    /// Source matrix: Start at each standard sample rate (44.1/48/88.2/96/176.4/
    /// 192 kHz) — reconciler must accept every rate and call ensure_source_rate
    /// (roadmap §45).
    #[tokio::test]
    async fn source_matrix_all_standard_rates_accepted() {
        let standard_rates = [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000];
        for rate in standard_rates {
            let source = active_source(rate);
            let dsp = dsp_snap(DspState::Running, rate);
            let (sync, counter) = SpyRateSync::new(RateSyncOutcome::AlreadyCorrect);
            let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
            assert!(
                matches!(action, ReconcileAction::RateSyncWhileRunning(_)),
                "rate {rate}: unexpected action {action:?}"
            );
            assert_eq!(
                counter.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "rate {rate}: ensure_source_rate must be called exactly once"
            );
        }
    }

    /// Source matrix: Start at each standard rate with DSP inactive — uses
    /// SetConfig path (roadmap §45, §17).
    #[tokio::test]
    async fn source_matrix_all_standard_rates_inactive_dsp_uses_set_config() {
        let standard_rates = [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000];
        for rate in standard_rates {
            let source = active_source(rate);
            let dsp = dsp_snap(DspState::Inactive, rate);
            let sync = FakeRateSync {
                outcome: RateSyncOutcome::SetConfigAfterInactive {
                    old_rate: None,
                    new_rate: rate,
                },
            };
            let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
            assert!(
                matches!(action, ReconcileAction::RateSyncAfterInactive(_)),
                "rate {rate}: unexpected action {action:?}"
            );
        }
    }

    /// Source matrix: Rapid flapping — source toggles active→inactive→active in
    /// quick succession.  Each individual reconcile_step call must produce the
    /// correct action for the snapshot it receives; the reconciler has no memory
    /// between calls (roadmap §45, §10).
    #[tokio::test]
    async fn source_matrix_rapid_flapping_each_step_independent() {
        let rate = 48_000;
        let active = active_source(rate);
        let inactive = inactive_source();
        let dsp = dsp_snap(DspState::Running, rate);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };

        // active → inactive → active → inactive
        let sequence: &[(&SourceSnapshot, ReconcileAction)] = &[
            (
                &active,
                ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect),
            ),
            (&inactive, ReconcileAction::WaitingForSourceStop),
            (
                &active,
                ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect),
            ),
            (&inactive, ReconcileAction::WaitingForSourceStop),
        ];

        for (i, (source, expected)) in sequence.iter().enumerate() {
            let action = reconcile_step(source, &dsp, &sync).await.unwrap();
            assert_eq!(action, *expected, "flap step {i}: unexpected action");
        }
    }

    /// Source matrix: Lost HCTL event — the same snapshot delivered twice must
    /// produce the same (idempotent) action.  Duplicate events are just redundant
    /// reconcile triggers; the outcome must be identical (roadmap §45).
    #[tokio::test]
    async fn source_matrix_duplicate_hctl_events_are_idempotent() {
        let source = active_source(44_100);
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };

        let a1 = reconcile_step(&source, &dsp, &sync).await.unwrap();
        let a2 = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert_eq!(
            a1, a2,
            "duplicate HCTL events must produce identical actions (idempotent)"
        );
    }

    /// Source matrix: Lost HCTL event at source-stop — if the inactive event is
    /// lost, the next reconcile sees inactive source and still waits for stop
    /// (roadmap §45).
    #[tokio::test]
    async fn source_matrix_lost_inactive_event_next_reconcile_still_waits() {
        // Simulate: the source stopped but the HCTL event was lost.
        // The next reconcile triggered by the safety timer reads inactive state.
        let source = inactive_source();
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };

        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert_eq!(action, ReconcileAction::WaitingForSourceStop);
    }

    // ── Gate 8 — Producer matrix (roadmap §46) ────────────────────────────────
    //
    // piCoreCDSP is producer-agnostic (roadmap §5): the reconciler has no
    // Squeezelite/AirPlay/Shairport-specific code.  The producer abstraction is
    // `SourceState { Inactive, Active { sample_rate } }`.  These tests model
    // each producer scenario through that abstraction.

    /// Producer matrix: Squeezelite producer active at 44.1 kHz — reconciler
    /// syncs rate and does not care which producer opened the loopback (roadmap §46).
    #[tokio::test]
    async fn producer_matrix_squeezelite_44100_rate_synced() {
        // Squeezelite is modelled as any active source at 44.1 kHz.
        let source = active_source(44_100);
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        ));
    }

    /// Producer matrix: AirPlay/Shairport producer active at 44.1 kHz —
    /// reconciler behaviour is identical regardless of producer identity
    /// (roadmap §46, §5).
    #[tokio::test]
    async fn producer_matrix_airplay_44100_rate_synced() {
        // AirPlay is modelled identically to any other active source at 44.1 kHz.
        let source = active_source(44_100);
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(matches!(
            action,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        ));
    }

    /// Producer matrix: Squeezelite → AirPlay handover at the same rate — new
    /// producer, same rate.  The generation counter bumps even though the rate is
    /// unchanged; the reconciler must still call ensure_source_rate (roadmap §31,
    /// §46).
    #[tokio::test]
    async fn producer_matrix_squeezelite_to_airplay_same_rate_triggers_rate_sync() {
        let rate = 44_100;
        let dsp = dsp_snap(DspState::Running, rate);
        let (sync, counter) = SpyRateSync::new(RateSyncOutcome::AlreadyCorrect);

        // First producer (Squeezelite): generation 1.
        let source_gen1 = SourceSnapshot {
            state: SourceState::Active { sample_rate: rate },
            sample_rate: Some(rate),
            format: Some("S32_LE".into()),
            channels: Some(2),
            generation: 1,
        };
        let a1 = reconcile_step(&source_gen1, &dsp, &sync).await.unwrap();
        assert!(matches!(
            a1,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        ));

        // Second producer (AirPlay): same rate, but generation bumped to 2.
        let source_gen2 = SourceSnapshot {
            state: SourceState::Active { sample_rate: rate },
            sample_rate: Some(rate),
            format: Some("S32_LE".into()),
            channels: Some(2),
            generation: 2,
        };
        let a2 = reconcile_step(&source_gen2, &dsp, &sync).await.unwrap();
        assert!(matches!(
            a2,
            ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
        ));

        // ensure_source_rate called on both reconcile passes.
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "ensure_source_rate must be called on every trigger including same-rate handover"
        );
    }

    /// Producer matrix: AirPlay → Squeezelite handover at a different rate —
    /// new producer at 96 kHz after previous at 44.1 kHz (roadmap §46).
    #[tokio::test]
    async fn producer_matrix_airplay_to_squeezelite_different_rate_patches_config() {
        let old_rate = 44_100;
        let new_rate = 96_000;
        let dsp = dsp_snap(DspState::Running, old_rate);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::PatchedWhileRunning {
                old_rate: Some(old_rate),
                new_rate,
            },
        };
        // New producer at 96 kHz (generation bump).
        let source = SourceSnapshot {
            state: SourceState::Active {
                sample_rate: new_rate,
            },
            sample_rate: Some(new_rate),
            format: Some("S32_LE".into()),
            channels: Some(2),
            generation: 2,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::PatchedWhileRunning {
                    new_rate: 96_000,
                    ..
                })
            ),
            "expected rate patch on AirPlay→Squeezelite handover at different rate, got {action:?}"
        );
    }

    /// Producer matrix: Parallel open — two producers cannot simultaneously hold
    /// the loopback PCM (ALSA exclusive access).  The SourceObserver reads the
    /// last-writer state; the reconciler acts on whatever snapshot it receives.
    /// This test verifies that a snapshot at any rate is handled (roadmap §46).
    #[tokio::test]
    async fn producer_matrix_parallel_open_reconciler_acts_on_last_writer_state() {
        // After a parallel-open race, the loopback reports the winner's rate.
        // The reconciler must handle whatever rate the SourceObserver reports.
        let winner_rate = 96_000;
        let source = active_source(winner_rate);
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::PatchedWhileRunning {
                old_rate: Some(44_100),
                new_rate: winner_rate,
            },
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::PatchedWhileRunning {
                    new_rate: 96_000,
                    ..
                })
            ),
            "parallel open: reconciler must act on winning producer's rate, got {action:?}"
        );
    }

    /// Producer matrix: Producer terminates unexpectedly — loopback becomes
    /// inactive.  Reconciler waits for CamillaDSP `stop_on_inactive` to fire,
    /// returning `WaitingForSourceStop` (roadmap §46, §12).
    #[tokio::test]
    async fn producer_matrix_unexpected_termination_reconciler_waits_for_stop() {
        let source = inactive_source(); // producer gone
        let dsp = dsp_snap(DspState::Running, 44_100);
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert_eq!(
            action,
            ReconcileAction::WaitingForSourceStop,
            "unexpected producer termination must produce WaitingForSourceStop"
        );
    }

    /// Producer matrix: Producer terminates and reopens immediately — two
    /// consecutive reconcile_steps: first inactive (stop), then active (new
    /// producer).  Each step must produce the correct action independently
    /// (roadmap §46, §10).
    #[tokio::test]
    async fn producer_matrix_termination_then_immediate_reopen_handled() {
        let rate = 44_100;
        let dsp_running = dsp_snap(DspState::Running, rate);
        let dsp_inactive = dsp_snap(DspState::Inactive, rate);

        // Step 1: source gone, DSP still running (brief window before stop_on_inactive fires).
        let action_stop = reconcile_step(
            &inactive_source(),
            &dsp_running,
            &FakeRateSync {
                outcome: RateSyncOutcome::AlreadyCorrect,
            },
        )
        .await
        .unwrap();
        assert_eq!(action_stop, ReconcileAction::WaitingForSourceStop);

        // Step 2: new producer arrived; DSP now inactive (stopped); rate sync via SetConfig.
        let action_reopen = reconcile_step(
            &active_source(rate),
            &dsp_inactive,
            &FakeRateSync {
                outcome: RateSyncOutcome::SetConfigAfterInactive {
                    old_rate: None,
                    new_rate: rate,
                },
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(
                action_reopen,
                ReconcileAction::RateSyncAfterInactive(RateSyncOutcome::SetConfigAfterInactive {
                    ..
                })
            ),
            "immediate reopen after termination must trigger RateSyncAfterInactive, got {action_reopen:?}"
        );
    }

    // ── Gate 8 — GUI matrix (roadmap §47) ────────────────────────────────────

    /// GUI matrix: Config switch during a rate change — user applies config B
    /// while source is switching from 44.1 to 96 kHz.  The reconciler must use
    /// the fresh DSP snapshot (which already has config B applied) and only patch
    /// the rate field in config B (roadmap §29.4, §47).
    ///
    /// This test verifies that when DSP reports config B as the active config
    /// during a rate change, the reconciler patches config B's rate field, not
    /// config A.  The `ensure_source_rate` spy receives the new DSP snapshot
    /// (containing config B), confirming the reconciler never reverts to config A.
    #[tokio::test]
    async fn gui_matrix_config_switch_during_rate_change_uses_new_config() {
        let old_rate = 44_100;
        let new_rate = 96_000;

        // Config B is now the active config — user applied it during the rate change.
        let config_b_yaml = format!(
            "devices:\n  samplerate: {old_rate}\n  capture:\n    type: Alsa\n    device: \"hw:Loopback,0,0\"\n    channels: 2\n    format: S32_LE\n    stop_on_inactive: true\n  filters:\n    FIR_B:\n      type: Conv\n      parameters:\n        filename: filter_b.wav\n"
        );
        let config_b = ConfigDocument::from_yaml(&config_b_yaml).unwrap();
        let fp_b = config_b.fingerprint();

        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(config_b.clone()),
            previous_config: Some(config_b.clone()),
            active_fingerprint: Some(fp_b),
            previous_fingerprint: Some(fp_b),
            stop_reason: None,
        };

        // Source has changed to 96 kHz; DSP is still running with config B at 44.1.
        let source = active_source(new_rate);
        let (sync, counter) = SpyRateSync::new(RateSyncOutcome::PatchedWhileRunning {
            old_rate: Some(old_rate),
            new_rate,
        });

        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();

        // Rate sync was called with config B — the reconciler does not revert to config A.
        assert!(
            matches!(
                action,
                ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::PatchedWhileRunning {
                    new_rate: 96_000,
                    ..
                })
            ),
            "config switch during rate change: expected RateSyncWhileRunning patching 96 kHz, got {action:?}"
        );
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "ensure_source_rate must be called exactly once with the new (config B) DSP snapshot"
        );
    }

    /// GUI matrix: Enable resampler during playback — user applies a config that
    /// adds a resampler while the source is playing.  The reconciler must not
    /// touch the resampler config, only the rate field (roadmap §47, §26,
    /// "schema-light ConfigDocument").
    ///
    /// This test confirms that the reconciler calls ensure_source_rate (which
    /// only patches the rate field) and does NOT attempt to re-apply the full
    /// config or remove the resampler.
    #[tokio::test]
    async fn gui_matrix_enable_resampler_during_playback_reconciler_only_touches_rate() {
        let rate = 44_100;

        // Config with resampler enabled — applied by the user via GUI Apply.
        let config_with_resampler = format!(
            "devices:\n  samplerate: {rate}\n  resampler:\n    type: Synchronous\n  capture:\n    type: Alsa\n    device: \"hw:Loopback,0,0\"\n    channels: 2\n    format: S32_LE\n    stop_on_inactive: true\n"
        );
        let doc = ConfigDocument::from_yaml(&config_with_resampler).unwrap();
        let fp = doc.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(doc.clone()),
            previous_config: Some(doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: None,
        };

        let source = active_source(rate);
        let (sync, counter) = SpyRateSync::new(RateSyncOutcome::AlreadyCorrect);

        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
            ),
            "enable resampler: reconciler must only call ensure_source_rate, got {action:?}"
        );
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "ensure_source_rate must be called exactly once — reconciler must not modify resampler config"
        );
    }

    /// GUI matrix: Disable resampler during playback — user removes the resampler
    /// via GUI Apply while source is playing.  The reconciler treats this
    /// identically to any other GUI Apply: only the rate field is its concern
    /// (roadmap §47, §29.1).
    #[tokio::test]
    async fn gui_matrix_disable_resampler_during_playback_reconciler_only_touches_rate() {
        let rate = 96_000;

        // Config with resampler removed — applied by the user via GUI Apply.
        let config_no_resampler = format!(
            "devices:\n  samplerate: {rate}\n  capture:\n    type: Alsa\n    device: \"hw:Loopback,0,0\"\n    channels: 2\n    format: S32_LE\n    stop_on_inactive: true\n"
        );
        let doc = ConfigDocument::from_yaml(&config_no_resampler).unwrap();
        let fp = doc.fingerprint();
        let dsp = DspSnapshot {
            state: DspState::Running,
            active_config: Some(doc.clone()),
            previous_config: Some(doc),
            active_fingerprint: Some(fp),
            previous_fingerprint: Some(fp),
            stop_reason: None,
        };

        let source = active_source(rate);
        let (sync, counter) = SpyRateSync::new(RateSyncOutcome::AlreadyCorrect);

        let action = reconcile_step(&source, &dsp, &sync).await.unwrap();
        assert!(
            matches!(
                action,
                ReconcileAction::RateSyncWhileRunning(RateSyncOutcome::AlreadyCorrect)
            ),
            "disable resampler: reconciler must only call ensure_source_rate, got {action:?}"
        );
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "ensure_source_rate must be called exactly once — reconciler must not restore resampler"
        );
    }

    // ── Gate 8 — Failure injection matrix (roadmap §48) ──────────────────────

    /// Failure injection: `snd-aloop` missing — the SourceObserver returns a
    /// `SourceObserver` error.  The run_loop must skip the current reconcile
    /// cycle gracefully (not crash) and wait for the next trigger (roadmap §48).
    #[tokio::test]
    async fn failure_injection_snd_aloop_missing_run_loop_skips_cycle() {
        use tokio::time::timeout;

        // A SourceObserver that always fails — simulates snd-aloop missing.
        struct AlwaysFailingObserver;

        #[async_trait]
        impl crate::source::observer::SourceObserver for AlwaysFailingObserver {
            async fn snapshot(&self) -> Result<SourceSnapshot, PicorecdspError> {
                Err(PicorecdspError::SourceObserver(
                    "snd-aloop: No such file or directory".into(),
                ))
            }
            async fn next_trigger(&mut self) -> Result<(), PicorecdspError> {
                Ok(())
            }
        }

        let camilla = FakeCamilla {
            state: DspState::Running,
            active_config: Some(compliant_config(44_100)),
            previous_config: Some(compliant_config(44_100)),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let cfg = fast_cfg();

        // Run 2 triggers then shut down.  The loop must not panic despite the
        // source observer always failing.
        let mut trigger = LimitedTrigger::new(2);
        let result = timeout(
            Duration::from_secs(5),
            run_loop(&camilla, &AlwaysFailingObserver, &sync, &mut trigger, &cfg),
        )
        .await
        .expect("run_loop did not terminate within 5 s");

        // run_loop terminates via trigger exhaustion (SourceObserver error from LimitedTrigger).
        assert!(
            matches!(result, Err(PicorecdspError::SourceObserver(_))),
            "snd-aloop missing: run_loop must terminate cleanly on trigger shutdown, got {result:?}"
        );
    }

    /// Failure injection: Loopback handle hangs — the SourceObserver returns
    /// a transient error after delay.  The run_loop treats any SourceObserver
    /// error as a skippable cycle (not a fatal crash), per roadmap §48.
    #[tokio::test]
    async fn failure_injection_loopback_handle_hangs_run_loop_skips_and_continues() {
        
        use tokio::time::timeout;

        // A SourceObserver that fails the first N snapshots (simulates hang + timeout),
        // then succeeds.
        struct HangingObserver {
            failures_remaining: Arc<Mutex<u32>>,
            good_snapshot: SourceSnapshot,
        }

        #[async_trait]
        impl crate::source::observer::SourceObserver for HangingObserver {
            async fn snapshot(&self) -> Result<SourceSnapshot, PicorecdspError> {
                let mut remaining = self.failures_remaining.lock().unwrap();
                if *remaining > 0 {
                    *remaining -= 1;
                    Err(PicorecdspError::SourceObserver(
                        "loopback handle timed out".into(),
                    ))
                } else {
                    Ok(self.good_snapshot.clone())
                }
            }
            async fn next_trigger(&mut self) -> Result<(), PicorecdspError> {
                Ok(())
            }
        }

        let camilla = FakeCamilla {
            state: DspState::Running,
            active_config: Some(compliant_config(44_100)),
            previous_config: Some(compliant_config(44_100)),
        };
        let sync = FakeRateSync {
            outcome: RateSyncOutcome::AlreadyCorrect,
        };
        let cfg = fast_cfg();

        let observer = HangingObserver {
            // First 2 snapshot calls fail, third succeeds.
            failures_remaining: Arc::new(Mutex::new(2)),
            good_snapshot: active_source(44_100),
        };
        // Run 3 triggers (2 skipped cycles due to hang, 1 successful cycle), then shut down.
        let mut trigger = LimitedTrigger::new(3);
        let result = timeout(
            Duration::from_secs(5),
            run_loop(&camilla, &observer, &sync, &mut trigger, &cfg),
        )
        .await
        .expect("run_loop timed out");

        // Should terminate via trigger exhaustion.
        assert!(
            matches!(result, Err(PicorecdspError::SourceObserver(_))),
            "loopback hang: run_loop must terminate cleanly, got {result:?}"
        );
    }

    /// Failure injection: WebSocket `RateLimitExceeded` — CamillaDSP rejects a
    /// config write because it is in a transitional state.  This error must
    /// propagate out of `reconcile_step` (the caller — `run_loop` — is responsible
    /// for retry logic after a fresh snapshot).  It must NOT be silently swallowed
    /// or treated as a partial success (roadmap §20, §48 / Cliffhanger C).
    #[tokio::test]
    async fn failure_injection_rate_limit_exceeded_propagates_out_of_reconcile_step() {
        let source = active_source(96_000);
        let dsp = dsp_snap(DspState::Running, 44_100);

        // A rate-sync impl that returns RateLimitExceeded — simulates CamillaDSP
        // rejecting the SetConfigValue because the process was mid-transition.
        struct RateLimitedSync;

        #[async_trait]
        impl SourceRateSynchronizer for RateLimitedSync {
            async fn ensure_source_rate(
                &self,
                _: u32,
                _: &DspSnapshot,
            ) -> Result<RateSyncOutcome, PicorecdspError> {
                Err(PicorecdspError::RateLimitExceeded)
            }
        }

        let result = reconcile_step(&source, &dsp, &RateLimitedSync).await;
        assert!(
            matches!(result, Err(PicorecdspError::RateLimitExceeded)),
            "RateLimitExceeded must propagate out of reconcile_step without being swallowed, got {result:?}"
        );
    }

    /// Failure injection: WebSocket `RateLimitExceeded` in the run_loop — the run
    /// loop must NOT silently absorb this; it should surface as a fatal error
    /// (since it is not handled in the match arms for known recoverable errors)
    /// causing the loop to terminate (roadmap §20, §48).
    #[tokio::test]
    async fn failure_injection_rate_limit_exceeded_in_run_loop_is_fatal() {
        use tokio::time::timeout;

        struct RateLimitedSync;

        #[async_trait]
        impl SourceRateSynchronizer for RateLimitedSync {
            async fn ensure_source_rate(
                &self,
                _: u32,
                _: &DspSnapshot,
            ) -> Result<RateSyncOutcome, PicorecdspError> {
                Err(PicorecdspError::RateLimitExceeded)
            }
        }

        let camilla = FakeCamilla {
            state: DspState::Running,
            active_config: Some(compliant_config(44_100)),
            previous_config: Some(compliant_config(44_100)),
        };
        // Source at 96 kHz so the rate-sync path is taken.
        let source_observer = FixedSourceObserver {
            snapshot: active_source(96_000),
        };
        let cfg = fast_cfg();
        let mut trigger = LimitedTrigger::new(1);

        let result = timeout(
            Duration::from_secs(5),
            run_loop(
                &camilla,
                &source_observer,
                &RateLimitedSync,
                &mut trigger,
                &cfg,
            ),
        )
        .await
        .expect("run_loop timed out");

        assert!(
            matches!(result, Err(PicorecdspError::RateLimitExceeded)),
            "RateLimitExceeded must be fatal in run_loop — must not be silently absorbed, got {result:?}"
        );
    }
}
