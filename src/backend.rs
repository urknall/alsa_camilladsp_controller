use crate::core::config::{DeviceSnapshot, WaveFormat};
use crate::core::errors::{app_error, AppResult};

pub mod aloop;
pub mod ioplug;

/// Identifies where stream parameters are detected for a backend.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamDetector {
    AloopHctl,
    IoplugIpc,
}

/// Identifies how PCM reaches CamillaDSP for a backend.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioTransport {
    AlsaCapture,
    StdinPipe,
}

/// Explicit backend profile tying detector and transport together.
#[allow(dead_code)]
pub trait BackendProfile {
    fn detector(&self) -> StreamDetector;
    fn transport(&self) -> AudioTransport;
}

/// Backend-neutral stream parameters used by the controller core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamParams {
    pub rate: u32,
    pub format: String,
    pub channels: u32,
}

impl StreamParams {
    /// Build stream parameters from a wave description that has all required fields.
    pub fn from_wave(wave: &WaveFormat) -> AppResult<Self> {
        let rate = wave
            .sample_rate
            .ok_or_else(|| app_error("missing sample rate"))?;
        let format = wave
            .sample_format
            .clone()
            .ok_or_else(|| app_error("missing sample format"))?;
        let channels = wave
            .channels
            .ok_or_else(|| app_error("missing channel count"))?;

        Ok(Self {
            rate,
            format,
            channels,
        })
    }
}

/// Backend-neutral stream lifecycle events emitted by stream backends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamEvent {
    Started(StreamParams),
    Changed(StreamParams),
    Stopped,
}

/// Event source abstraction for aloop/ioplug stream detectors.
#[allow(dead_code)]
pub trait StreamBackend {
    fn next_event(&mut self) -> AppResult<StreamEvent>;
}

/// Interface used by the controller state machine to drive a stream backend.
///
/// Extends the basic event source with a non-blocking poll, access to the
/// last-observed stream state, and a live snapshot refresh used when
/// CamillaDSP reports a capture format change.  Both `AloopBackend` and the
/// future `IoplugBackend` must implement this trait so the controller core
/// can remain backend-neutral.
pub trait ControllerBackend {
    /// Poll for a stream event without blocking longer than `timeout_ms`.
    ///
    /// Returns `Some(event)` when a state transition is detected, `None` when
    /// the poll period elapsed without a change.
    fn poll_event(&mut self, timeout_ms: u32) -> AppResult<Option<StreamEvent>>;

    /// Return a reference to the stream state observed during the last
    /// `poll_event` call.  The returned snapshot is valid until the next call
    /// to `poll_event`.
    fn current_snapshot(&self) -> &DeviceSnapshot;

    /// Perform a live re-read of the current stream state from the underlying
    /// source (ALSA controls, IPC socket, etc.).  Used by the controller to
    /// recover accurate stream parameters after a CamillaDSP
    /// `CaptureFormatChange` stop reason.
    fn read_snapshot(&self) -> AppResult<DeviceSnapshot>;

    /// Called by the controller after CamillaDSP has been successfully prepared
    /// for a new stream.  For the `aloop` backend this is a no-op.  For the
    /// `ioplug` backend this sends the `READY` message to the plugin, releasing
    /// it to start transferring PCM to CamillaDSP.
    fn on_stream_ready(&mut self) -> AppResult<()> {
        Ok(())
    }
}

/// Detect a backend-neutral stream lifecycle event from consecutive snapshots.
pub fn detect_stream_event(
    previous: &DeviceSnapshot,
    current: &DeviceSnapshot,
    fallback_wave: &WaveFormat,
) -> AppResult<Option<StreamEvent>> {
    let event = if !previous.active && current.active {
        let params = StreamParams::from_wave(&current.wave.with_fallback(fallback_wave))?;
        Some(StreamEvent::Started(params))
    } else if previous.active && !current.active {
        Some(StreamEvent::Stopped)
    } else if previous.active && current.active && previous.wave != current.wave {
        let params = StreamParams::from_wave(&current.wave.with_fallback(fallback_wave))?;
        Some(StreamEvent::Changed(params))
    } else {
        None
    };

    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_params_from_wave_succeeds_with_complete_wave() {
        let wave = WaveFormat {
            sample_rate: Some(96_000),
            sample_format: Some("S24_4_LE".to_owned()),
            channels: Some(2),
        };

        let params = StreamParams::from_wave(&wave).unwrap();
        assert_eq!(params.rate, 96_000);
        assert_eq!(params.format, "S24_4_LE");
        assert_eq!(params.channels, 2);
    }

    #[test]
    fn stream_params_from_wave_fails_when_rate_is_missing() {
        let wave = WaveFormat {
            sample_rate: None,
            sample_format: Some("S16_LE".to_owned()),
            channels: Some(2),
        };

        assert!(StreamParams::from_wave(&wave).is_err());
    }

    #[test]
    fn stream_params_from_wave_fails_when_format_is_missing() {
        let wave = WaveFormat {
            sample_rate: Some(44_100),
            sample_format: None,
            channels: Some(2),
        };

        assert!(StreamParams::from_wave(&wave).is_err());
    }

    #[test]
    fn stream_params_from_wave_fails_when_channels_are_missing() {
        let wave = WaveFormat {
            sample_rate: Some(44_100),
            sample_format: Some("S16_LE".to_owned()),
            channels: None,
        };

        assert!(StreamParams::from_wave(&wave).is_err());
    }

    fn active_snapshot(rate: u32, format: &str, channels: u32) -> DeviceSnapshot {
        DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: Some(rate),
                sample_format: Some(format.to_owned()),
                channels: Some(channels),
            },
        }
    }

    #[test]
    fn detect_stream_event_started() {
        let previous = DeviceSnapshot {
            active: false,
            wave: WaveFormat::default(),
        };
        let current = active_snapshot(48_000, "S32_LE", 2);
        let fallback = WaveFormat::default();

        let event = detect_stream_event(&previous, &current, &fallback).unwrap();
        assert_eq!(
            event,
            Some(StreamEvent::Started(StreamParams {
                rate: 48_000,
                format: "S32_LE".to_owned(),
                channels: 2,
            }))
        );
    }

    #[test]
    fn detect_stream_event_changed() {
        let previous = active_snapshot(44_100, "S16_LE", 2);
        let current = active_snapshot(96_000, "S24_4_LE", 2);
        let fallback = WaveFormat::default();

        let event = detect_stream_event(&previous, &current, &fallback).unwrap();
        assert_eq!(
            event,
            Some(StreamEvent::Changed(StreamParams {
                rate: 96_000,
                format: "S24_4_LE".to_owned(),
                channels: 2,
            }))
        );
    }

    #[test]
    fn detect_stream_event_stopped() {
        let previous = active_snapshot(44_100, "S16_LE", 2);
        let current = DeviceSnapshot {
            active: false,
            wave: WaveFormat::default(),
        };
        let fallback = WaveFormat::default();

        let event = detect_stream_event(&previous, &current, &fallback).unwrap();
        assert_eq!(event, Some(StreamEvent::Stopped));
    }

    #[test]
    fn detect_stream_event_uses_fallback_for_missing_fields() {
        let previous = active_snapshot(44_100, "S16_LE", 2);
        let current = DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: Some(48_000),
                sample_format: None,
                channels: None,
            },
        };
        let fallback = WaveFormat {
            sample_rate: Some(44_100),
            sample_format: Some("S32_LE".to_owned()),
            channels: Some(2),
        };

        let event = detect_stream_event(&previous, &current, &fallback).unwrap();
        assert_eq!(
            event,
            Some(StreamEvent::Changed(StreamParams {
                rate: 48_000,
                format: "S32_LE".to_owned(),
                channels: 2,
            }))
        );
    }

    #[test]
    fn detect_stream_event_none_when_no_transition() {
        let previous = active_snapshot(44_100, "S16_LE", 2);
        let current = previous.clone();
        let fallback = WaveFormat::default();

        let event = detect_stream_event(&previous, &current, &fallback).unwrap();
        assert_eq!(event, None);
    }
}

// ─── Phase 13 — Cross-backend behavioural test suite ─────────────────────────
//
// These tests verify that the state machine's event-handling and adaptation
// pipeline produce correct results when driven by abstract `StreamEvent`
// inputs, independent of whether those events came from `AloopBackend` or
// `IoplugBackend`.
//
// Design:
// * `MockStreamBackend` implements `ControllerBackend` and emits a pre-loaded
//   queue of `StreamEvent`s.  It stands in for both real backends, because
//   the only contract between the state machine and a backend is the
//   `ControllerBackend` trait.
// * `adapt_for_event` is a helper that turns a `StreamEvent` into a runtime
//   CamillaDSP config string via `adapt_config_for_backend`, mimicking what
//   the real controller does inside `handle_started` / `handle_changed`.
// * Each test checks the output for both `RuntimeBackend::Aloop` and
//   `RuntimeBackend::Ioplug`.
//
// Behavioural invariants verified by this suite:
//   Started(44100, S16_LE, 2) -> aloop config has Alsa capture, rate 44100
//   Started(44100, S16_LE, 2) -> ioplug config has Stdin capture, rate 44100
//   Changed(48000, S24_3_LE, 2) -> both backends update samplerate to 48000
//   Stopped -> does not trigger adaptation (no config change)
//   Same result regardless of backend that emitted the event
#[cfg(test)]
mod cross_backend_tests {
    use super::*;
    use crate::core::adaptation::{adapt_config_for_backend, RuntimeBackend};
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    // ── Fixtures ─────────────────────────────────────────────────────────

    fn test_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "picoredsp-phase13-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Minimal portable baseline config (no capture section) that is valid
    /// for both backends.  Mirrors `portable_base_config` in adaptation tests.
    fn portable_config(playback: &str) -> String {
        format!(
            "devices:\n\
             \x20 samplerate: 44100\n\
             \x20 chunksize: 1024\n\
             \x20 playback:\n\
             \x20   type: Alsa\n\
             \x20   channels: 2\n\
             \x20   device: \"{playback}\"\n\
             filters: {{}}\n\
             mixers: {{}}\n\
             pipeline: []\n\
             processors: {{}}\n"
        )
    }

    /// Convert `StreamParams` to the `WaveFormat` used by adaptation.
    fn params_to_wave(params: &StreamParams) -> WaveFormat {
        WaveFormat {
            sample_rate: Some(params.rate),
            sample_format: Some(params.format.clone()),
            channels: Some(params.channels),
        }
    }

    /// Adapt a config for both backends and return (aloop_yaml, ioplug_yaml).
    fn adapt_both(config_path: &std::path::Path, wave: &WaveFormat) -> (String, String) {
        let aloop = adapt_config_for_backend(config_path, wave, RuntimeBackend::Aloop).unwrap();
        let ioplug = adapt_config_for_backend(config_path, wave, RuntimeBackend::Ioplug).unwrap();
        (aloop, ioplug)
    }

    // ── Mock backend ─────────────────────────────────────────────────────

    /// A minimal `ControllerBackend` that emits pre-scripted `StreamEvent`s.
    ///
    /// Represents either an `AloopBackend` or an `IoplugBackend` at the
    /// trait level — the state machine cannot tell them apart.
    struct MockStreamBackend {
        events: VecDeque<StreamEvent>,
        snapshot: DeviceSnapshot,
    }

    impl MockStreamBackend {
        fn new(events: Vec<StreamEvent>) -> Self {
            Self {
                events: events.into(),
                snapshot: DeviceSnapshot {
                    active: false,
                    wave: WaveFormat::default(),
                },
            }
        }

        fn next(&mut self) -> Option<StreamEvent> {
            let ev = self.events.pop_front()?;
            // Update the internal snapshot to reflect the emitted event.
            match &ev {
                StreamEvent::Started(p) | StreamEvent::Changed(p) => {
                    self.snapshot = DeviceSnapshot {
                        active: true,
                        wave: WaveFormat {
                            sample_rate: Some(p.rate),
                            sample_format: Some(p.format.clone()),
                            channels: Some(p.channels),
                        },
                    };
                }
                StreamEvent::Stopped => {
                    self.snapshot = DeviceSnapshot {
                        active: false,
                        wave: WaveFormat::default(),
                    };
                }
            }
            Some(ev)
        }
    }

    impl ControllerBackend for MockStreamBackend {
        fn poll_event(&mut self, _timeout_ms: u32) -> AppResult<Option<StreamEvent>> {
            Ok(self.next())
        }

        fn current_snapshot(&self) -> &DeviceSnapshot {
            &self.snapshot
        }

        fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
            Ok(self.snapshot.clone())
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    /// Started(44100, S16_LE, 2) -> correct runtime config for both backends.
    ///
    /// Aloop: capture type=Alsa, device=hw:Loopback,0,0, samplerate=44100,
    ///        stop_on_inactive=true, no explicit format.
    /// Ioplug: capture type=Stdin, format=S16_LE, samplerate=44100, no device.
    #[test]
    fn started_44100_s16_2_produces_correct_aloop_and_ioplug_configs() {
        let dir = test_dir("started-44100");
        let config_path = dir.join("config.yml");
        fs::write(&config_path, portable_config("hw:DAC,0")).unwrap();

        let mut backend = MockStreamBackend::new(vec![StreamEvent::Started(StreamParams {
            rate: 44_100,
            format: "S16_LE".to_owned(),
            channels: 2,
        })]);

        let event = backend.poll_event(0).unwrap().unwrap();
        let params = match &event {
            StreamEvent::Started(p) => p,
            _ => panic!("expected Started"),
        };

        let wave = params_to_wave(params);
        let (aloop, ioplug) = adapt_both(&config_path, &wave);

        // Aloop checks
        let al: serde_yaml_ng::Value = serde_yaml_ng::from_str(&aloop).unwrap();
        assert_eq!(al["devices"]["samplerate"].as_u64(), Some(44_100));
        assert_eq!(al["devices"]["capture"]["type"].as_str(), Some("Alsa"));
        assert_eq!(
            al["devices"]["capture"]["device"].as_str(),
            Some("hw:Loopback,0,0")
        );
        assert_eq!(
            al["devices"]["capture"]["stop_on_inactive"].as_bool(),
            Some(true)
        );
        assert!(al["devices"]["capture"].get("format").is_none());

        // Ioplug checks
        let io: serde_yaml_ng::Value = serde_yaml_ng::from_str(&ioplug).unwrap();
        assert_eq!(io["devices"]["samplerate"].as_u64(), Some(44_100));
        assert_eq!(io["devices"]["capture"]["type"].as_str(), Some("Stdin"));
        assert_eq!(io["devices"]["capture"]["format"].as_str(), Some("S16_LE"));
        assert!(io["devices"]["capture"].get("device").is_none());
        assert!(io["devices"]["capture"].get("stop_on_inactive").is_none());

        // Playback device must be identical for both backends
        assert_eq!(
            al["devices"]["playback"]["device"].as_str(),
            io["devices"]["playback"]["device"].as_str()
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Changed(48000, S24_3_LE, 2) -> both backends update samplerate to 48000
    /// and the appropriate capture section.
    #[test]
    fn changed_48000_s24_3le_2_updates_samplerate_for_both_backends() {
        let dir = test_dir("changed-48000");
        let config_path = dir.join("config.yml");
        fs::write(&config_path, portable_config("hw:DAC,0")).unwrap();

        let mut backend = MockStreamBackend::new(vec![
            StreamEvent::Started(StreamParams {
                rate: 44_100,
                format: "S16_LE".to_owned(),
                channels: 2,
            }),
            StreamEvent::Changed(StreamParams {
                rate: 48_000,
                format: "S24_3_LE".to_owned(),
                channels: 2,
            }),
        ]);

        // Consume Started
        let _ = backend.poll_event(0).unwrap().unwrap();

        // Consume Changed
        let event = backend.poll_event(0).unwrap().unwrap();
        let params = match &event {
            StreamEvent::Changed(p) => p,
            _ => panic!("expected Changed"),
        };

        let wave = params_to_wave(params);
        let (aloop, ioplug) = adapt_both(&config_path, &wave);

        let al: serde_yaml_ng::Value = serde_yaml_ng::from_str(&aloop).unwrap();
        let io: serde_yaml_ng::Value = serde_yaml_ng::from_str(&ioplug).unwrap();

        assert_eq!(al["devices"]["samplerate"].as_u64(), Some(48_000));
        assert_eq!(io["devices"]["samplerate"].as_u64(), Some(48_000));

        assert_eq!(al["devices"]["capture"]["type"].as_str(), Some("Alsa"));
        assert_eq!(io["devices"]["capture"]["type"].as_str(), Some("Stdin"));
        assert_eq!(
            io["devices"]["capture"]["format"].as_str(),
            Some("S24_3_LE")
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Stopped -> backend snapshot becomes inactive; no adaptation is triggered.
    /// Verifies that the mock backend correctly tracks inactive state after Stop.
    #[test]
    fn stopped_transitions_backend_to_inactive_state() {
        let mut backend = MockStreamBackend::new(vec![
            StreamEvent::Started(StreamParams {
                rate: 48_000,
                format: "S32_LE".to_owned(),
                channels: 2,
            }),
            StreamEvent::Stopped,
        ]);

        // After Started, snapshot is active.
        let _ = backend.poll_event(0).unwrap().unwrap();
        assert!(backend.current_snapshot().active);

        // After Stopped, snapshot is inactive.
        let ev = backend.poll_event(0).unwrap().unwrap();
        assert_eq!(ev, StreamEvent::Stopped);
        assert!(!backend.current_snapshot().active);
    }

    /// Verifies that the same `StreamEvent::Started` value produces identical
    /// adaptation results regardless of which backend it is assumed to come from.
    /// (The actual backend is irrelevant to adaptation — it only affects the
    /// runtime config's capture section.)
    #[test]
    fn same_event_produces_same_adaptation_for_aloop_and_ioplug_snapshots() {
        let dir = test_dir("same-event");
        let config_path = dir.join("config.yml");
        fs::write(&config_path, portable_config("hw:Speaker,0")).unwrap();

        let params = StreamParams {
            rate: 96_000,
            format: "S32_LE".to_owned(),
            channels: 2,
        };

        // Simulate two mock backends (one aloop-like, one ioplug-like) emitting
        // the same event.
        let event = StreamEvent::Started(params.clone());
        assert_eq!(event, StreamEvent::Started(params.clone()));

        let wave = params_to_wave(&params);

        let (aloop1, ioplug1) = adapt_both(&config_path, &wave);
        // Re-adapt with the same wave to confirm determinism.
        let (aloop2, ioplug2) = adapt_both(&config_path, &wave);

        assert_eq!(aloop1, aloop2, "aloop adaptation must be deterministic");
        assert_eq!(ioplug1, ioplug2, "ioplug adaptation must be deterministic");

        // Both should have the same samplerate and playback device.
        let al: serde_yaml_ng::Value = serde_yaml_ng::from_str(&aloop1).unwrap();
        let io: serde_yaml_ng::Value = serde_yaml_ng::from_str(&ioplug1).unwrap();
        assert_eq!(al["devices"]["samplerate"], io["devices"]["samplerate"]);
        assert_eq!(
            al["devices"]["playback"]["device"],
            io["devices"]["playback"]["device"]
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Verifies all six standard sample rates produce the correct samplerate
    /// field in the adapted config for both backends.
    #[test]
    fn all_standard_rates_produce_correct_samplerate_in_adapted_config() {
        let dir = test_dir("all-rates");
        let config_path = dir.join("config.yml");
        fs::write(&config_path, portable_config("hw:X,0")).unwrap();

        let rates = [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000];
        let formats = ["S16_LE", "S24_3_LE", "S24_4_LE", "S32_LE", "F32_LE"];

        for &rate in &rates {
            let wave = WaveFormat {
                sample_rate: Some(rate),
                sample_format: Some("S32_LE".to_owned()),
                channels: Some(2),
            };
            let (aloop, ioplug) = adapt_both(&config_path, &wave);
            let al: serde_yaml_ng::Value = serde_yaml_ng::from_str(&aloop).unwrap();
            let io: serde_yaml_ng::Value = serde_yaml_ng::from_str(&ioplug).unwrap();
            assert_eq!(
                al["devices"]["samplerate"].as_u64(),
                Some(u64::from(rate)),
                "aloop samplerate mismatch at {rate}"
            );
            assert_eq!(
                io["devices"]["samplerate"].as_u64(),
                Some(u64::from(rate)),
                "ioplug samplerate mismatch at {rate}"
            );
        }

        // All formats must produce the correct format field for ioplug's
        // `Stdin` capture device. `S24_4_LE` is CamillaDSP's `Alsa`-schema
        // name and must be translated to the generic-schema name
        // `S24_4_RJ_LE` for `Stdin` (verified against CamillaDSP 4.1.3 — see
        // `alsa_only_format_to_generic`); every other format name is shared
        // between both schemas and passes through unchanged.
        for &fmt in &formats {
            let wave = WaveFormat {
                sample_rate: Some(48_000),
                sample_format: Some(fmt.to_owned()),
                channels: Some(2),
            };
            let ioplug =
                adapt_config_for_backend(&config_path, &wave, RuntimeBackend::Ioplug).unwrap();
            let io: serde_yaml_ng::Value = serde_yaml_ng::from_str(&ioplug).unwrap();
            let expected = if fmt == "S24_4_LE" {
                "S24_4_RJ_LE"
            } else {
                fmt
            };
            assert_eq!(
                io["devices"]["capture"]["format"].as_str(),
                Some(expected),
                "ioplug format mismatch for {fmt}"
            );
        }

        fs::remove_dir_all(dir).unwrap();
    }

    /// `detect_stream_event` produces identical results when the same before/after
    /// snapshot pair is observed by either backend.  This confirms the event
    /// detection layer is fully decoupled from the aloop HCTL implementation.
    #[test]
    fn detect_stream_event_is_backend_neutral() {
        let inactive = DeviceSnapshot {
            active: false,
            wave: WaveFormat::default(),
        };
        let active_44 = DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: Some(44_100),
                sample_format: Some("S16_LE".to_owned()),
                channels: Some(2),
            },
        };
        let active_48 = DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: Some(48_000),
                sample_format: Some("S24_3_LE".to_owned()),
                channels: Some(2),
            },
        };
        let fallback = WaveFormat::default();

        // Started
        assert_eq!(
            detect_stream_event(&inactive, &active_44, &fallback).unwrap(),
            Some(StreamEvent::Started(StreamParams {
                rate: 44_100,
                format: "S16_LE".to_owned(),
                channels: 2,
            }))
        );

        // Changed
        assert_eq!(
            detect_stream_event(&active_44, &active_48, &fallback).unwrap(),
            Some(StreamEvent::Changed(StreamParams {
                rate: 48_000,
                format: "S24_3_LE".to_owned(),
                channels: 2,
            }))
        );

        // Stopped
        assert_eq!(
            detect_stream_event(&active_44, &inactive, &fallback).unwrap(),
            Some(StreamEvent::Stopped)
        );

        // No transition
        assert_eq!(
            detect_stream_event(&active_44, &active_44, &fallback).unwrap(),
            None
        );
    }
}
