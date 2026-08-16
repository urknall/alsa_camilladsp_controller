//! CamillaDSP transport contract and managed-mode state (roadmap
//! `piCoreCDSP_v2_Roadmap.md` §7, §9, §26).
//!
//! Rust reads a CamillaDSP config and validates only the small, schema-light set of
//! paths it needs (roadmap §26): `devices.capture.type`, `devices.capture.device`,
//! `devices.capture.channels`, `devices.capture.format`, and
//! `devices.capture.stop_on_inactive`. It never models filters, mixer, pipeline, or
//! any other part of the config, and it never repairs, rewrites, or writes back the
//! user's YAML (roadmap §9): on an incompatible config it suspends managed mode with a
//! clear error and waits for the user to fix it.

use serde::Deserialize;

/// The schema-light slice of `devices.capture.*` that the transport contract cares
/// about (roadmap §7, §26). Any other part of the CamillaDSP config is deliberately
/// left unparsed.
#[derive(Debug, Clone, Deserialize)]
struct DevicesSection {
    capture: CaptureSection,
}

#[derive(Debug, Clone, Deserialize)]
struct CaptureSection {
    #[serde(rename = "type")]
    capture_type: String,
    device: String,
    channels: u32,
    format: String,
    #[serde(default)]
    stop_on_inactive: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfigRoot {
    devices: DevicesSection,
}

/// The capture transport fields extracted from a CamillaDSP config, reduced to
/// exactly what the transport contract (roadmap §7) validates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTransportConfig {
    pub capture_type: String,
    pub device: String,
    pub channels: u32,
    pub format: String,
    pub stop_on_inactive: bool,
}

/// The config could be parsed as YAML but was missing or malformed at one of the
/// known `devices.capture.*` paths this crate reads. This is a read failure, not a
/// contract violation: it means Rust cannot even determine whether the transport
/// contract is satisfied.
#[derive(Debug)]
pub struct ConfigReadError(String);

impl std::fmt::Display for ConfigReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to read CamillaDSP config at the known devices.capture.* paths: {}",
            self.0
        )
    }
}

impl std::error::Error for ConfigReadError {}

/// Reads (never repairs or rewrites) the `devices.capture.*` fields from raw
/// CamillaDSP config YAML, per the schema-light `ConfigDocument` philosophy (roadmap
/// §26). Any other part of the document is ignored.
pub fn read_capture_transport_config(
    config_yaml: &str,
) -> Result<CaptureTransportConfig, ConfigReadError> {
    let root: ConfigRoot =
        serde_norway::from_str(config_yaml).map_err(|e| ConfigReadError(e.to_string()))?;
    Ok(CaptureTransportConfig {
        capture_type: root.devices.capture.capture_type,
        device: root.devices.capture.device,
        channels: root.devices.capture.channels,
        format: root.devices.capture.format,
        stop_on_inactive: root.devices.capture.stop_on_inactive,
    })
}

/// The required values of the CamillaDSP transport contract (roadmap §7).
pub const EXPECTED_CAPTURE_TYPE: &str = "Alsa";
pub const EXPECTED_CAPTURE_DEVICE: &str = "hw:Loopback,0,0";
pub const EXPECTED_CAPTURE_CHANNELS: u32 = 2;
pub const EXPECTED_CAPTURE_FORMAT: &str = "S32_LE";

/// One invariant of the CamillaDSP transport contract (roadmap §7) that a config
/// failed to satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportContractViolation {
    UnexpectedCaptureType {
        expected: &'static str,
        actual: String,
    },
    UnexpectedCaptureDevice {
        expected: &'static str,
        actual: String,
    },
    UnexpectedChannels {
        expected: u32,
        actual: u32,
    },
    UnexpectedFormat {
        expected: &'static str,
        actual: String,
    },
    StopOnInactiveDisabled,
}

impl std::fmt::Display for TransportContractViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportContractViolation::UnexpectedCaptureType { expected, actual } => {
                write!(
                    f,
                    "devices.capture.type must be `{expected}`, found `{actual}`"
                )
            }
            TransportContractViolation::UnexpectedCaptureDevice { expected, actual } => {
                write!(
                    f,
                    "devices.capture.device must be `{expected}`, found `{actual}`"
                )
            }
            TransportContractViolation::UnexpectedChannels { expected, actual } => {
                write!(
                    f,
                    "devices.capture.channels must be {expected}, found {actual}"
                )
            }
            TransportContractViolation::UnexpectedFormat { expected, actual } => {
                write!(
                    f,
                    "devices.capture.format must be `{expected}`, found `{actual}`"
                )
            }
            TransportContractViolation::StopOnInactiveDisabled => {
                write!(f, "devices.capture.stop_on_inactive must be true")
            }
        }
    }
}

impl std::error::Error for TransportContractViolation {}

/// Validates a [`CaptureTransportConfig`] against the CamillaDSP transport contract
/// (roadmap §7). This function only reads and validates: it never mutates the config
/// or produces a "repaired" version, per roadmap §9's hard config invariants. Returns
/// every violation found so a single clear error can be reported to the user.
pub fn validate_transport_contract(
    cfg: &CaptureTransportConfig,
) -> Result<(), Vec<TransportContractViolation>> {
    let mut violations = Vec::new();

    if cfg.capture_type != EXPECTED_CAPTURE_TYPE {
        violations.push(TransportContractViolation::UnexpectedCaptureType {
            expected: EXPECTED_CAPTURE_TYPE,
            actual: cfg.capture_type.clone(),
        });
    }
    if cfg.device != EXPECTED_CAPTURE_DEVICE {
        violations.push(TransportContractViolation::UnexpectedCaptureDevice {
            expected: EXPECTED_CAPTURE_DEVICE,
            actual: cfg.device.clone(),
        });
    }
    if cfg.channels != EXPECTED_CAPTURE_CHANNELS {
        violations.push(TransportContractViolation::UnexpectedChannels {
            expected: EXPECTED_CAPTURE_CHANNELS,
            actual: cfg.channels,
        });
    }
    if cfg.format != EXPECTED_CAPTURE_FORMAT {
        violations.push(TransportContractViolation::UnexpectedFormat {
            expected: EXPECTED_CAPTURE_FORMAT,
            actual: cfg.format.clone(),
        });
    }
    if !cfg.stop_on_inactive {
        violations.push(TransportContractViolation::StopOnInactiveDisabled);
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Whether piCoreCDSP is actively reconciling ALSA and CamillaDSP, or has suspended
/// itself because the current CamillaDSP config is not transport-contract compatible
/// (roadmap §7: "Managed mode suspended → clear error → wait for user change").
///
/// Entering [`ManagedMode::Suspended`] is the *only* reaction Rust is allowed to have
/// to an incompatible config: it never repairs, overwrites, or auto-corrects the
/// user's YAML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedMode {
    /// The transport contract is satisfied; reconciliation may proceed.
    Active,
    /// The transport contract is violated. `reason` is the clear, user-facing error
    /// describing what to change; Rust takes no corrective action on its own.
    Suspended { reason: String },
}

/// Derives the [`ManagedMode`] that should result from validating a capture transport
/// config, formatting all violations into a single clear, human-readable reason.
pub fn managed_mode_for(cfg: &CaptureTransportConfig) -> ManagedMode {
    match validate_transport_contract(cfg) {
        Ok(()) => ManagedMode::Active,
        Err(violations) => {
            let reason = violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            ManagedMode::Suspended { reason }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPLIANT_CONFIG: &str = r#"
devices:
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
"#;

    #[test]
    fn compliant_config_reads_and_validates() {
        let cfg = read_capture_transport_config(COMPLIANT_CONFIG).expect("must parse");
        assert_eq!(cfg.capture_type, "Alsa");
        assert_eq!(cfg.device, "hw:Loopback,0,0");
        assert_eq!(cfg.channels, 2);
        assert_eq!(cfg.format, "S32_LE");
        assert!(cfg.stop_on_inactive);
        assert_eq!(validate_transport_contract(&cfg), Ok(()));
        assert_eq!(managed_mode_for(&cfg), ManagedMode::Active);
    }

    #[test]
    fn wrong_device_suspends_managed_mode_with_clear_reason() {
        let yaml = COMPLIANT_CONFIG.replace("hw:Loopback,0,0", "hw:Loopback,1,0");
        let cfg = read_capture_transport_config(&yaml).unwrap();
        let violations = validate_transport_contract(&cfg).unwrap_err();
        assert_eq!(
            violations,
            vec![TransportContractViolation::UnexpectedCaptureDevice {
                expected: "hw:Loopback,0,0",
                actual: "hw:Loopback,1,0".into(),
            }]
        );
        match managed_mode_for(&cfg) {
            ManagedMode::Suspended { reason } => assert!(reason.contains("devices.capture.device")),
            ManagedMode::Active => panic!("expected managed mode to suspend"),
        }
    }

    #[test]
    fn stop_on_inactive_false_is_a_violation() {
        let yaml = COMPLIANT_CONFIG.replace("stop_on_inactive: true", "stop_on_inactive: false");
        let cfg = read_capture_transport_config(&yaml).unwrap();
        assert_eq!(
            validate_transport_contract(&cfg).unwrap_err(),
            vec![TransportContractViolation::StopOnInactiveDisabled]
        );
    }

    #[test]
    fn missing_stop_on_inactive_defaults_to_false_and_is_a_violation() {
        let yaml = r#"
devices:
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
"#;
        let cfg = read_capture_transport_config(yaml).unwrap();
        assert_eq!(
            validate_transport_contract(&cfg).unwrap_err(),
            vec![TransportContractViolation::StopOnInactiveDisabled]
        );
    }

    #[test]
    fn every_violation_is_reported_together() {
        let yaml = r#"
devices:
  capture:
    type: File
    device: "hw:Loopback,2,0"
    channels: 1
    format: S16_LE
    stop_on_inactive: false
"#;
        let cfg = read_capture_transport_config(yaml).unwrap();
        let violations = validate_transport_contract(&cfg).unwrap_err();
        assert_eq!(violations.len(), 5);
    }

    #[test]
    fn malformed_config_is_a_read_error_not_a_panic_or_silent_default() {
        let yaml = "devices: {}";
        let err = read_capture_transport_config(yaml).unwrap_err();
        assert!(err.to_string().contains("devices.capture"));
    }
}
