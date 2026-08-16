//! Schema-light typed view into a [`ConfigDocument`] (roadmap §26).
//!
//! [`ConfigView`] sits between the raw YAML tree ([`ConfigDocument`]) and the
//! reconciler / rate-sync code. It exposes only the paths that Rust is authorised
//! to read (§26), with typed getters, so that callers never manipulate raw
//! dot-path strings directly and so that the boundary between "Rust knows about
//! this" and "Rust carries this through untouched" is enforced at the type level.
//!
//! # Paths modelled (§26)
//!
//! ```text
//! devices.samplerate
//! devices.capture_samplerate
//! devices.resampler
//! devices.capture.type
//! devices.capture.device
//! devices.capture.channels
//! devices.capture.format
//! devices.capture.stop_on_inactive
//! ```
//!
//! # What is intentionally NOT modelled here
//!
//! Filters, mixer, pipeline, processors, FIR paths, biquad definitions — all
//! user-owned configuration that Rust must never inspect or rewrite. Those
//! subtrees are carried through [`ConfigDocument`] untouched.
//!
//! No wire-format details (WebSocket command names, JSON envelope shapes,
//! version-specific quirks) appear in this module (roadmap §24).

use crate::camilla::config_document::ConfigDocument;

// ── Well-known config paths (roadmap §26) ────────────────────────────────────

/// `devices.samplerate` — the DSP output sample rate.
///
/// This field is the rate-sync target in the **no-resampler** case (roadmap §15).
/// Rust only writes this field; it never writes any other rate-related field
/// in the no-resampler path.
pub const PATH_SAMPLERATE: &str = "devices.samplerate";

/// `devices.capture_samplerate` — the capture (source) sample rate.
///
/// This field is the rate-sync target in the **resampler** case (roadmap §15).
/// Only `devices.capture_samplerate` is patched when a resampler is configured;
/// `devices.samplerate` remains user-owned in that path.
pub const PATH_CAPTURE_SAMPLERATE: &str = "devices.capture_samplerate";

/// `devices.resampler` — presence indicates that a resampler is configured.
///
/// When this key is a non-null mapping, `PATH_CAPTURE_SAMPLERATE` is the
/// appropriate rate-sync target; otherwise `PATH_SAMPLERATE` is used.
pub const PATH_RESAMPLER: &str = "devices.resampler";

/// `devices.capture.type` — the ALSA capture device type string.
///
/// Used to verify the capture device is the expected loopback type.
pub const PATH_CAPTURE_TYPE: &str = "devices.capture.type";

/// `devices.capture.device` — the ALSA capture device name.
///
/// Used to verify the capture device matches the expected loopback device.
pub const PATH_CAPTURE_DEVICE: &str = "devices.capture.device";

/// `devices.capture.channels` — the capture channel count.
///
/// Used during loopback compliance checks (§44 installer verification).
pub const PATH_CAPTURE_CHANNELS: &str = "devices.capture.channels";

/// `devices.capture.format` — the capture sample format string.
///
/// Used during loopback compliance checks (§44 installer verification).
pub const PATH_CAPTURE_FORMAT: &str = "devices.capture.format";

/// `devices.capture.stop_on_inactive` — whether CamillaDSP stops on silence.
///
/// Must be `true` in a compliant piCoreCDSP config so that `GetPreviousConfig`
/// is reliably populated after the source goes inactive (roadmap §17, §44).
pub const PATH_STOP_ON_INACTIVE: &str = "devices.capture.stop_on_inactive";

// ── Typed view ───────────────────────────────────────────────────────────────

/// A read-only, schema-light view of the paths in a [`ConfigDocument`] that
/// Rust is authorised to inspect (roadmap §26).
///
/// `ConfigView` borrows a `ConfigDocument` and exposes typed accessors for the
/// specific paths listed in §26. All other config content remains opaque and
/// is never inspected through this type.
///
/// # No wire-format details
///
/// `ConfigView` contains no WebSocket command names, JSON envelope shapes, or
/// version-specific protocol details.  Those live exclusively in
/// `camilla/protocol_v4.rs` and `camilla/protocol_v5.rs` (roadmap §24).
#[derive(Debug)]
pub struct ConfigView<'a> {
    doc: &'a ConfigDocument,
}

impl<'a> ConfigView<'a> {
    /// Create a typed view over an existing [`ConfigDocument`].
    pub fn new(doc: &'a ConfigDocument) -> Self {
        Self { doc }
    }

    /// The DSP output sample rate (`devices.samplerate`), if present and numeric.
    pub fn samplerate(&self) -> Option<u32> {
        self.doc
            .get(PATH_SAMPLERATE)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
    }

    /// The capture sample rate (`devices.capture_samplerate`), if present and numeric.
    pub fn capture_samplerate(&self) -> Option<u32> {
        self.doc
            .get(PATH_CAPTURE_SAMPLERATE)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
    }

    /// Whether a resampler is configured (`devices.resampler` is a non-null mapping).
    ///
    /// When `true`, rate-sync must target `PATH_CAPTURE_SAMPLERATE`; when `false`,
    /// rate-sync targets `PATH_SAMPLERATE`.
    pub fn has_resampler(&self) -> bool {
        self.doc.has_resampler()
    }

    /// The appropriate rate-sync target path for this config (roadmap §15).
    ///
    /// Returns `PATH_CAPTURE_SAMPLERATE` when a resampler is configured,
    /// `PATH_SAMPLERATE` otherwise.
    pub fn rate_field_path(&self) -> &'static str {
        self.doc.rate_field_path()
    }

    /// The current rate in the rate-sync target field.
    ///
    /// Equivalent to reading the value at [`Self::rate_field_path()`].
    pub fn effective_source_rate(&self) -> Option<u32> {
        if self.has_resampler() {
            self.capture_samplerate()
        } else {
            self.samplerate()
        }
    }

    /// The capture device type string (`devices.capture.type`), if present.
    pub fn capture_type(&self) -> Option<&str> {
        self.doc.get(PATH_CAPTURE_TYPE).and_then(|v| v.as_str())
    }

    /// The capture device name (`devices.capture.device`), if present.
    pub fn capture_device(&self) -> Option<&str> {
        self.doc.get(PATH_CAPTURE_DEVICE).and_then(|v| v.as_str())
    }

    /// The capture channel count (`devices.capture.channels`), if present and numeric.
    pub fn capture_channels(&self) -> Option<u32> {
        self.doc
            .get(PATH_CAPTURE_CHANNELS)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
    }

    /// The capture sample format string (`devices.capture.format`), if present.
    pub fn capture_format(&self) -> Option<&str> {
        self.doc.get(PATH_CAPTURE_FORMAT).and_then(|v| v.as_str())
    }

    /// Whether `stop_on_inactive` is set to `true` (`devices.capture.stop_on_inactive`).
    ///
    /// Returns `false` when the field is absent, `null`, or `false`.
    pub fn stop_on_inactive(&self) -> bool {
        self.doc
            .get(PATH_STOP_ON_INACTIVE)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const YAML_NO_RESAMPLER: &str = "\
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: \"hw:Loopback,0,0\"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
";

    const YAML_WITH_RESAMPLER: &str = "\
devices:
  samplerate: 96000
  capture_samplerate: 44100
  resampler:
    type: BalancedAsync
  capture:
    type: Alsa
    device: \"hw:Loopback,0,0\"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
";

    #[test]
    fn no_resampler_reads_samplerate() {
        let doc = ConfigDocument::from_yaml(YAML_NO_RESAMPLER).unwrap();
        let view = ConfigView::new(&doc);
        assert_eq!(view.samplerate(), Some(44100));
        assert_eq!(view.capture_samplerate(), None);
        assert!(!view.has_resampler());
        assert_eq!(view.rate_field_path(), PATH_SAMPLERATE);
        assert_eq!(view.effective_source_rate(), Some(44100));
    }

    #[test]
    fn resampler_reads_capture_samplerate() {
        let doc = ConfigDocument::from_yaml(YAML_WITH_RESAMPLER).unwrap();
        let view = ConfigView::new(&doc);
        assert_eq!(view.capture_samplerate(), Some(44100));
        assert!(view.has_resampler());
        assert_eq!(view.rate_field_path(), PATH_CAPTURE_SAMPLERATE);
        assert_eq!(view.effective_source_rate(), Some(44100));
    }

    #[test]
    fn capture_device_fields_are_readable() {
        let doc = ConfigDocument::from_yaml(YAML_NO_RESAMPLER).unwrap();
        let view = ConfigView::new(&doc);
        assert_eq!(view.capture_type(), Some("Alsa"));
        assert_eq!(view.capture_device(), Some("hw:Loopback,0,0"));
        assert_eq!(view.capture_channels(), Some(2));
        assert_eq!(view.capture_format(), Some("S32_LE"));
        assert!(view.stop_on_inactive());
    }

    #[test]
    fn stop_on_inactive_absent_returns_false() {
        const YAML: &str = "\
devices:
  samplerate: 48000
  capture:
    type: Alsa
    device: \"hw:Loopback,0,0\"
    channels: 2
    format: S32_LE
";
        let doc = ConfigDocument::from_yaml(YAML).unwrap();
        let view = ConfigView::new(&doc);
        assert!(!view.stop_on_inactive());
    }
}
