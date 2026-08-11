use std::fmt;

/// The audio format reported by the ALSA loopback or USB gadget control interface.
///
/// Any field may be `None` when the underlying ALSA control does not expose that
/// information (e.g. the USB gadget source only reports sample rate).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WaveFormat {
    pub sample_rate: Option<u32>,
    /// CamillaDSP format name string (e.g. `"S32_LE"`, `"S24_4_RJ_LE"`).
    pub sample_format: Option<String>,
    pub channels: Option<u32>,
}

impl fmt::Display for WaveFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rate={:?}, format={:?}, channels={:?}",
            self.sample_rate, self.sample_format, self.channels
        )
    }
}

impl WaveFormat {
    /// Return a new `WaveFormat` where any `None` fields in `self` are filled
    /// from the corresponding field in `fallback`.
    pub fn with_fallback(&self, fallback: &WaveFormat) -> Self {
        Self {
            sample_rate: self.sample_rate.or(fallback.sample_rate),
            sample_format: self
                .sample_format
                .clone()
                .or_else(|| fallback.sample_format.clone()),
            channels: self.channels.or(fallback.channels),
        }
    }
}

/// A point-in-time snapshot of the loopback device state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSnapshot {
    pub active: bool,
    pub wave: WaveFormat,
}
