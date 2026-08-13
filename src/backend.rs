use crate::error::{app_error, AppResult};
use crate::wave::WaveFormat;

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
pub trait StreamBackend {
    fn next_event(&mut self) -> AppResult<StreamEvent>;
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
}
