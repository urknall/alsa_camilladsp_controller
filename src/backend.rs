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
