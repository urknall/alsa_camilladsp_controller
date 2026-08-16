//! [`ConfigPatchRateSynchronizer`] — the current workaround for the lack of a
//! native CamillaDSP source-rate override (roadmap §16–§17 / Cliffhanger A).
//!
//! # What this does
//!
//! * **While DSP is Running/Paused** (roadmap §16): call `SetConfigValue` on the
//!   single rate field (`devices.samplerate` or `devices.capture_samplerate`).
//!   This is a targeted patch that does not touch any user-owned field.
//!
//! * **After DSP settled Inactive** (roadmap §17): read `GetPreviousConfig` fresh,
//!   patch the single rate field with `with_path_value`, and call `SetConfig`.
//!   This re-applies the full previous config with the rate updated — preserving
//!   filters, mixer, pipeline, Apply-without-Save, and any GUI changes.
//!
//! # What this does NOT do
//!
//! * Never touch `devices.samplerate` in the resampler case (that field stays
//!   user-owned; only `devices.capture_samplerate` is patched).
//! * Never write `runtime.yml` or any shadow config.
//! * Never revert a GUI Apply or a config switch.
//! * Never cache the config across reconcile iterations.
//!
//! # Removal criterion (Cliffhanger A)
//!
//! `upstream/capabilities.yml` key: `persistent_source_rate_override`.  This
//! entire file is deleted when upstream provides a persistent source-rate override
//! that satisfies all the conditions listed in the roadmap §18.

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    camilla::control::CamillaControl,
    error::PicorecdspError,
    rate_sync::{DspSnapshot, RateSyncOutcome, SourceRateSynchronizer},
};

/// The config-patch implementation of [`SourceRateSynchronizer`] (Cliffhanger A).
///
/// Pass a reference to whatever [`CamillaControl`] implementation is in use
/// (v4 or v5 adapter); this struct does not hold state between calls.
pub struct ConfigPatchRateSynchronizer<'a> {
    camilla: &'a (dyn CamillaControl + 'a),
}

impl<'a> ConfigPatchRateSynchronizer<'a> {
    pub fn new(camilla: &'a dyn CamillaControl) -> Self {
        Self { camilla }
    }
}

#[async_trait]
impl SourceRateSynchronizer for ConfigPatchRateSynchronizer<'_> {
    async fn ensure_source_rate(
        &self,
        source_rate: u32,
        dsp: &DspSnapshot,
    ) -> Result<RateSyncOutcome, PicorecdspError> {
        let config = match dsp.authoritative_config() {
            Some(c) => c,
            None => {
                return Err(PicorecdspError::ProtocolError(
                    "ensure_source_rate called with no authoritative config".into(),
                ))
            }
        };

        // Gate: refuse to patch a config that uses $samplerate$ tokens.
        if config.has_samplerate_token() {
            return Err(PicorecdspError::SamplerateTokenGuard {
                detail: "detected in authoritative config".into(),
            });
        }

        let rate_path = config.rate_field_path();
        let current_rate = config
            .get(rate_path)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        if current_rate == Some(source_rate) {
            return Ok(RateSyncOutcome::AlreadyCorrect);
        }

        if dsp.state.is_active() {
            // DSP is Running or Paused → use SetConfigValue for a targeted patch.
            self.camilla
                .set_config_value(rate_path, Value::Number(source_rate.into()))
                .await?;
            return Ok(RateSyncOutcome::PatchedWhileRunning {
                old_rate: current_rate,
                new_rate: source_rate,
            });
        }

        // DSP is settled Inactive → read PreviousConfig fresh (no long-lived cache),
        // patch the rate field, call SetConfig.
        //
        // We re-read PreviousConfig here rather than using `config` from the
        // snapshot because the snapshot may have been taken a moment ago and a
        // concurrent GUI Apply could have changed it (Cliffhanger C mitigation:
        // fresh read immediately before write).
        let fresh_previous = self.camilla.previous_config().await?.ok_or_else(|| {
            PicorecdspError::ProtocolError(
                "ensure_source_rate: DSP inactive but no PreviousConfig available".into(),
            )
        })?;

        if fresh_previous.has_samplerate_token() {
            return Err(PicorecdspError::SamplerateTokenGuard {
                detail: "detected in fresh PreviousConfig".into(),
            });
        }

        let rate_path = fresh_previous.rate_field_path();
        let old_rate = fresh_previous
            .get(rate_path)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let patched = fresh_previous
            .with_path_value(rate_path, Value::Number(source_rate.into()))
            .map_err(|e| PicorecdspError::ConfigRead(e.to_string()))?;

        self.camilla.set_config(&patched).await?;

        Ok(RateSyncOutcome::SetConfigAfterInactive {
            old_rate,
            new_rate: source_rate,
        })
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
        rate_sync::DspSnapshot,
    };
    use async_trait::async_trait;
    use serde_json::Value;
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    // ── Test-double CamillaControl ──────────────────────────────────────────

    #[derive(Default)]
    struct FakeCamilla {
        set_config_value_calls: Arc<Mutex<Vec<(String, Value)>>>,
        set_config_calls: Arc<Mutex<Vec<ConfigDocument>>>,
        previous_config: Option<ConfigDocument>,
    }

    impl FakeCamilla {
        fn with_previous_config(mut self, yaml: &str) -> Self {
            self.previous_config = Some(ConfigDocument::from_yaml(yaml).unwrap());
            self
        }
    }

    #[async_trait]
    impl CamillaControl for FakeCamilla {
        async fn version(&self) -> Result<Version, PicorecdspError> {
            Ok(Version::new(4, 2, 0))
        }
        async fn state(&self) -> Result<DspState, PicorecdspError> {
            Ok(DspState::Inactive)
        }
        async fn stop_reason(&self) -> Result<Option<StopReason>, PicorecdspError> {
            Ok(None)
        }
        async fn active_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
            Ok(None)
        }
        async fn previous_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
            Ok(self.previous_config.clone())
        }
        async fn config_file_path(&self) -> Result<Option<PathBuf>, PicorecdspError> {
            Ok(None)
        }
        async fn set_config(&self, config: &ConfigDocument) -> Result<(), PicorecdspError> {
            self.set_config_calls.lock().unwrap().push(config.clone());
            Ok(())
        }
        async fn set_config_value(&self, path: &str, value: Value) -> Result<(), PicorecdspError> {
            self.set_config_value_calls
                .lock()
                .unwrap()
                .push((path.to_owned(), value));
            Ok(())
        }
        async fn stop(&self) -> Result<(), PicorecdspError> {
            Ok(())
        }
    }

    fn config_yaml(rate: u32) -> String {
        format!(
            "devices:\n  samplerate: {rate}\n  capture:\n    type: Alsa\n    device: \"hw:Loopback,0,0\"\n    channels: 2\n    format: S32_LE\n    stop_on_inactive: true\n"
        )
    }

    fn active_snap(state: DspState, rate: u32) -> DspSnapshot {
        let doc = ConfigDocument::from_yaml(&config_yaml(rate)).unwrap();
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

    // ── Tests ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn already_correct_produces_no_write() {
        let camilla = FakeCamilla::default();
        let set_value_calls = camilla.set_config_value_calls.clone();
        let set_config_calls = camilla.set_config_calls.clone();
        let sync = ConfigPatchRateSynchronizer::new(&camilla);
        let snap = active_snap(DspState::Running, 44_100);
        let outcome = sync.ensure_source_rate(44_100, &snap).await.unwrap();
        assert_eq!(outcome, RateSyncOutcome::AlreadyCorrect);
        assert!(set_value_calls.lock().unwrap().is_empty());
        assert!(set_config_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn running_dsp_uses_set_config_value() {
        let camilla = FakeCamilla::default();
        let set_value_calls = camilla.set_config_value_calls.clone();
        let sync = ConfigPatchRateSynchronizer::new(&camilla);
        let snap = active_snap(DspState::Running, 44_100);
        let outcome = sync.ensure_source_rate(96_000, &snap).await.unwrap();
        assert!(matches!(
            outcome,
            RateSyncOutcome::PatchedWhileRunning {
                new_rate: 96_000,
                ..
            }
        ));
        let calls = set_value_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "devices.samplerate");
        assert_eq!(calls[0].1, Value::Number(96_000.into()));
    }

    #[tokio::test]
    async fn inactive_dsp_uses_set_config() {
        let prev_yaml = config_yaml(44_100);
        let camilla = FakeCamilla::default().with_previous_config(&prev_yaml);
        let set_config_calls = camilla.set_config_calls.clone();
        let sync = ConfigPatchRateSynchronizer::new(&camilla);

        let doc = ConfigDocument::from_yaml(&prev_yaml).unwrap();
        let fp = doc.fingerprint();
        let snap = DspSnapshot {
            state: DspState::Inactive,
            active_config: None,
            previous_config: Some(doc),
            active_fingerprint: None,
            previous_fingerprint: Some(fp),
            stop_reason: None,
        };

        let outcome = sync.ensure_source_rate(96_000, &snap).await.unwrap();
        assert!(matches!(
            outcome,
            RateSyncOutcome::SetConfigAfterInactive {
                new_rate: 96_000,
                ..
            }
        ));
        let calls = set_config_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("devices.samplerate"),
            Some(&Value::Number(96_000.into()))
        );
    }

    /// Mandatory regression: gain +6 dB applied without save must survive a
    /// source rate change 44.1 → 96 → 48 kHz (roadmap §30, §47).
    #[tokio::test]
    async fn apply_without_save_gain_survives_rate_change() {
        let gain_yaml = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
filters:
  gain_filter:
    type: Gain
    parameters:
      gain: 6.0
"#;
        // FakeCamilla returns this as PreviousConfig (simulating GUI Apply, no Save).
        let camilla = FakeCamilla::default().with_previous_config(gain_yaml);
        let set_config_calls = camilla.set_config_calls.clone();
        let sync = ConfigPatchRateSynchronizer::new(&camilla);

        let doc = ConfigDocument::from_yaml(gain_yaml).unwrap();
        let snap = DspSnapshot {
            state: DspState::Inactive,
            active_config: None,
            previous_config: Some(doc.clone()),
            active_fingerprint: None,
            previous_fingerprint: Some(doc.fingerprint()),
            stop_reason: None,
        };

        // Simulate 44.1 → 96 kHz.
        sync.ensure_source_rate(96_000, &snap).await.unwrap();
        {
            let calls = set_config_calls.lock().unwrap();
            let patched = &calls[0];
            // Rate was updated.
            assert_eq!(
                patched.get("devices.samplerate"),
                Some(&Value::Number(96_000.into()))
            );
            // Gain was preserved (never user-owned fields touched).
            assert_eq!(
                patched.get("filters.gain_filter.parameters.gain"),
                Some(&Value::Number(serde_json::Number::from_f64(6.0).unwrap()))
            );
        }
    }

    #[tokio::test]
    async fn samplerate_token_is_fail_closed() {
        let token_yaml = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
filters:
  fir:
    type: Conv
    parameters:
      filename: "fir_$samplerate$.wav"
"#;
        let doc = ConfigDocument::from_yaml(token_yaml).unwrap();
        let snap = DspSnapshot {
            state: DspState::Running,
            active_config: Some(doc.clone()),
            previous_config: Some(doc.clone()),
            active_fingerprint: Some(doc.fingerprint()),
            previous_fingerprint: Some(doc.fingerprint()),
            stop_reason: None,
        };
        let camilla = FakeCamilla::default();
        let sync = ConfigPatchRateSynchronizer::new(&camilla);
        let err = sync.ensure_source_rate(96_000, &snap).await.unwrap_err();
        assert!(matches!(err, PicorecdspError::SamplerateTokenGuard { .. }));
    }

    /// Save-without-Apply (roadmap §29.3):
    /// When the user saves the config file (updating the disk file) without
    /// clicking Apply, the running DSP state is unchanged.  piCoreCDSP's rate
    /// sync must not auto-reload from disk; it uses only `GetPreviousConfig`
    /// (the applied runtime config) for the rate patch.
    ///
    /// Verification: `ConfigPatchRateSynchronizer` only calls `previous_config()`
    /// on `CamillaControl` — it never reads from a file path.  The `FakeCamilla`
    /// returns only the PreviousConfig it was constructed with, and the rate patch
    /// uses that — regardless of what might be on disk.
    #[tokio::test]
    async fn save_without_apply_rate_sync_ignores_disk_config() {
        // The applied (previous) config has a gain filter at 44.1 kHz.
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
  eq:
    type: Gain
    parameters:
      gain: 6.0
"#;
        // A different config was saved to disk (but NOT applied).
        // piCoreCDSP has no disk watcher and never reads from the file path.
        // The FakeCamilla simulates this: previous_config returns the APPLIED
        // config, and the config_file_path (if queried) would return a path that
        // points to a different file.  But config_file_path is never queried by
        // ConfigPatchRateSynchronizer, so only the applied config is used.
        let camilla = FakeCamilla::default().with_previous_config(applied_yaml);
        let set_config_calls = camilla.set_config_calls.clone();
        let sync = ConfigPatchRateSynchronizer::new(&camilla);

        let doc = ConfigDocument::from_yaml(applied_yaml).unwrap();
        let snap = DspSnapshot {
            state: DspState::Inactive,
            active_config: None,
            previous_config: Some(doc.clone()),
            active_fingerprint: None,
            previous_fingerprint: Some(doc.fingerprint()),
            stop_reason: None,
        };

        // New source at 96 kHz.
        let outcome = sync.ensure_source_rate(96_000, &snap).await.unwrap();
        assert!(matches!(
            outcome,
            RateSyncOutcome::SetConfigAfterInactive {
                new_rate: 96_000,
                ..
            }
        ));

        let calls = set_config_calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "exactly one SetConfig must be issued");
        let patched = &calls[0];
        // Rate was updated from the APPLIED config, not any disk file.
        assert_eq!(
            patched.get("devices.samplerate"),
            Some(&Value::Number(96_000.into()))
        );
        // Gain from the APPLIED config was preserved (Save-without-Apply is irrelevant).
        assert_eq!(
            patched.get("filters.eq.parameters.gain"),
            Some(&Value::Number(serde_json::Number::from_f64(6.0).unwrap()))
        );
    }

    /// Apply-without-Save regression (roadmap §29.2, §30, §47):
    /// A gain setting applied via GUI Apply (but not Saved) must survive a
    /// complete source rate cycle.  This is the mandatory regression test from
    /// roadmap §30, §47.  See also `apply_without_save_gain_survives_rate_change`
    /// above for the single-step version; this test covers the full cycle
    /// 44.1 → 96 → 48 kHz as required by the roadmap.
    #[tokio::test]
    async fn apply_without_save_gain_survives_full_rate_cycle() {
        let gain_yaml = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
filters:
  gain_filter:
    type: Gain
    parameters:
      gain: 6.0
"#;
        // After each rate change the FakeCamilla's previous_config is the patched
        // config from the prior step.  We simulate this by re-constructing the
        // FakeCamilla with the patched config at each step.

        // ── Step 1: 44.1 → 96 kHz ────────────────────────────────────────────
        let camilla = FakeCamilla::default().with_previous_config(gain_yaml);
        let set_config_calls_1 = camilla.set_config_calls.clone();
        let sync = ConfigPatchRateSynchronizer::new(&camilla);

        let doc = ConfigDocument::from_yaml(gain_yaml).unwrap();
        let snap = DspSnapshot {
            state: DspState::Inactive,
            active_config: None,
            previous_config: Some(doc.clone()),
            active_fingerprint: None,
            previous_fingerprint: Some(doc.fingerprint()),
            stop_reason: None,
        };
        sync.ensure_source_rate(96_000, &snap).await.unwrap();

        let patched_96_yaml = {
            let calls_1 = set_config_calls_1.lock().unwrap();
            let patched_96 = &calls_1[0];
            assert_eq!(
                patched_96.get("devices.samplerate"),
                Some(&Value::Number(96_000.into())),
                "rate must be 96 kHz after first patch"
            );
            assert_eq!(
                patched_96.get("filters.gain_filter.parameters.gain"),
                Some(&Value::Number(serde_json::Number::from_f64(6.0).unwrap())),
                "gain must survive 44.1 → 96 kHz"
            );
            patched_96.to_yaml().unwrap()
        };
        let _ = sync;
        drop(camilla);

        let camilla2 = FakeCamilla::default().with_previous_config(&patched_96_yaml);
        let set_config_calls_2 = camilla2.set_config_calls.clone();
        let sync2 = ConfigPatchRateSynchronizer::new(&camilla2);

        let doc2 = ConfigDocument::from_yaml(&patched_96_yaml).unwrap();
        let snap2 = DspSnapshot {
            state: DspState::Inactive,
            active_config: None,
            previous_config: Some(doc2.clone()),
            active_fingerprint: None,
            previous_fingerprint: Some(doc2.fingerprint()),
            stop_reason: None,
        };
        sync2.ensure_source_rate(48_000, &snap2).await.unwrap();

        let calls_2 = set_config_calls_2.lock().unwrap();
        let patched_48 = &calls_2[0];
        assert_eq!(
            patched_48.get("devices.samplerate"),
            Some(&Value::Number(48_000.into())),
            "rate must be 48 kHz after second patch"
        );
        assert_eq!(
            patched_48.get("filters.gain_filter.parameters.gain"),
            Some(&Value::Number(serde_json::Number::from_f64(6.0).unwrap())),
            "gain must survive 96 → 48 kHz"
        );
    }
}
