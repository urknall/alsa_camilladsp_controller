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

/// Selects how the controller prepares CamillaDSP's configuration before
/// starting processing.
///
/// The split is made once, at the single choke point where the controller
/// decides how to start CamillaDSP (`Controller::start_cdsp_with_wave` for
/// `aloop`, the equivalent dispatch in `run_ioplug` for `ioplug`) — no other
/// code path is meant to branch on the mode.
///
/// * [`ConfigMode::Static`] — the active config file (e.g.
///   `active_config.yml`) is loaded byte-for-byte, unmodified: no
///   `adapt_config`/`adapt_config_for_backend`, no runtime YAML, no
///   `SetConfig` with mutated content. What the user sees in CamillaGUI is
///   exactly what CamillaDSP runs. If the detected transport `WaveFormat`
///   does not match the config, that is surfaced as a read-only diagnostic
///   log line (see `core::adaptation::diagnose_static_config_mismatch`) —
///   never "fixed" by rewriting the config. This is the only mode the
///   installer selects.
/// * [`ConfigMode::Adaptive`] — the original behavior: the baseline config is
///   adapted in memory for the detected `WaveFormat` and backend transport
///   (see `core::adaptation`) before being sent to CamillaDSP. Retained in
///   full — including `adapt_config`, `adapt_config_for_backend`, and the
///   transient runtime-config file — as a developer/legacy mode; the
///   installer never selects it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConfigMode {
    /// Load the active config file unmodified. Installer default.
    Static,
    /// Adapt the config in memory for the detected transport (legacy).
    #[default]
    Adaptive,
}

impl ConfigMode {
    /// Parse a `--config-mode` CLI value (`"static"` or `"adaptive"`).
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "static" => Ok(Self::Static),
            "adaptive" => Ok(Self::Adaptive),
            other => Err(format!(
                "--config-mode must be 'static' or 'adaptive', got '{other}'"
            )),
        }
    }
}

impl fmt::Display for ConfigMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Static => "static",
            Self::Adaptive => "adaptive",
        })
    }
}

#[cfg(test)]
mod config_mode_tests {
    use super::ConfigMode;

    #[test]
    fn parse_accepts_static() {
        assert_eq!(ConfigMode::parse("static"), Ok(ConfigMode::Static));
    }

    #[test]
    fn parse_accepts_adaptive() {
        assert_eq!(ConfigMode::parse("adaptive"), Ok(ConfigMode::Adaptive));
    }

    #[test]
    fn parse_rejects_unknown_value() {
        let err = ConfigMode::parse("bogus").unwrap_err();
        assert!(err.contains("static"));
        assert!(err.contains("adaptive"));
        assert!(err.contains("bogus"));
    }

    #[test]
    fn default_is_adaptive_for_backward_compatibility() {
        // The installer always passes `--config-mode static` explicitly
        // (see `install_picoredsp.sh`); the in-binary default stays
        // `Adaptive` so any existing manual/CI invocation without the flag
        // keeps behaving exactly as it did before `ConfigMode` existed.
        assert_eq!(ConfigMode::default(), ConfigMode::Adaptive);
    }

    #[test]
    fn display_round_trips_through_parse() {
        for mode in [ConfigMode::Static, ConfigMode::Adaptive] {
            assert_eq!(ConfigMode::parse(&mode.to_string()), Ok(mode));
        }
    }
}
