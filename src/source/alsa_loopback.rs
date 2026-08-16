//! ALSA ingress contract (roadmap `piCoreCDSP_v2_Roadmap.md` §6).
//!
//! Defines the canonical `pcm.picorecdsp` `plug` definition that every producer must
//! go through on its way to `snd-aloop`, and a validator that checks a parsed
//! definition against the contract's invariants: `format = S32_LE`,
//! `channels = 2`, `samplerate = unchanged`.
//!
//! This module only *describes and validates* the contract. Writing the definition to
//! `/etc/asound.conf` (or equivalent) is an installer concern (roadmap Gate 11), not a
//! runtime Rust concern.

/// The canonical `pcm.picorecdsp` ALSA `plug` definition (roadmap §6), targeting the
/// first `snd-aloop` playback subdevice as the shared ingress point for every
/// producer.
pub const CANONICAL_ASOUND_CONF: &str = r#"pcm.picorecdsp {
    type plug
    slave {
        pcm "hw:Loopback,1,0"
        format S32_LE
        channels 2
        rate unchanged
    }
}
"#;

/// The transport rate policy of an ALSA `plug` slave, as far as piCoreCDSP cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RatePolicy {
    /// `rate unchanged` — no resampling is performed by the plug; the negotiated rate
    /// passes through untouched. This is the only policy the roadmap allows.
    Unchanged,
    /// Any other explicit rate value or policy keyword.
    Other(String),
}

/// A parsed ALSA `plug` definition, reduced to the fields the roadmap's ALSA ingress
/// contract cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlsaPlugDefinition {
    pub slave_pcm: String,
    pub format: String,
    pub channels: u32,
    pub rate_policy: RatePolicy,
}

/// A required field was missing from the `plug` definition being parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlsaConfigParseError {
    pub missing_field: &'static str,
}

impl std::fmt::Display for AlsaConfigParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ALSA plug definition is missing required field `{}`",
            self.missing_field
        )
    }
}

impl std::error::Error for AlsaConfigParseError {}

/// One invariant of the ALSA ingress contract (roadmap §6) that a definition failed to
/// satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlsaContractViolation {
    UnexpectedFormat {
        expected: &'static str,
        actual: String,
    },
    UnexpectedChannels {
        expected: u32,
        actual: u32,
    },
    UnexpectedRatePolicy {
        actual: RatePolicy,
    },
}

impl std::fmt::Display for AlsaContractViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlsaContractViolation::UnexpectedFormat { expected, actual } => {
                write!(f, "expected format `{expected}`, found `{actual}`")
            }
            AlsaContractViolation::UnexpectedChannels { expected, actual } => {
                write!(f, "expected {expected} channels, found {actual}")
            }
            AlsaContractViolation::UnexpectedRatePolicy { actual } => {
                write!(f, "expected `rate unchanged`, found `{actual:?}`")
            }
        }
    }
}

impl std::error::Error for AlsaContractViolation {}

const EXPECTED_FORMAT: &str = "S32_LE";
const EXPECTED_CHANNELS: u32 = 2;

/// Parses the `slave { ... }` block of a `pcm.picorecdsp` `plug` definition.
///
/// This is intentionally a minimal, purpose-built parser for our own canonical block
/// shape (roadmap §26's "schema-light" philosophy applied to ALSA config too) — it is
/// not a general-purpose `asound.conf` parser.
pub fn parse_slave_block(conf_text: &str) -> Result<AlsaPlugDefinition, AlsaConfigParseError> {
    let mut slave_pcm: Option<String> = None;
    let mut format: Option<String> = None;
    let mut channels: Option<u32> = None;
    let mut rate_policy: Option<RatePolicy> = None;

    for raw_line in conf_text.lines() {
        let line = raw_line.trim().trim_end_matches(',');
        if let Some(rest) = line.strip_prefix("pcm ") {
            slave_pcm = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("format ") {
            format = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("channels ") {
            channels = rest.trim().parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix("rate ") {
            let value = rest.trim();
            rate_policy = Some(if value == "unchanged" {
                RatePolicy::Unchanged
            } else {
                RatePolicy::Other(value.to_string())
            });
        }
    }

    Ok(AlsaPlugDefinition {
        slave_pcm: slave_pcm.ok_or(AlsaConfigParseError {
            missing_field: "slave.pcm",
        })?,
        format: format.ok_or(AlsaConfigParseError {
            missing_field: "slave.format",
        })?,
        channels: channels.ok_or(AlsaConfigParseError {
            missing_field: "slave.channels",
        })?,
        rate_policy: rate_policy.ok_or(AlsaConfigParseError {
            missing_field: "slave.rate",
        })?,
    })
}

/// Validates a parsed `plug` definition against the ALSA ingress contract's invariants
/// (roadmap §6): `format = S32_LE`, `channels = 2`, `rate = unchanged`. Returns every
/// violation found rather than stopping at the first one, so a single report can be
/// surfaced to the user.
pub fn validate_alsa_contract(def: &AlsaPlugDefinition) -> Result<(), Vec<AlsaContractViolation>> {
    let mut violations = Vec::new();

    if def.format != EXPECTED_FORMAT {
        violations.push(AlsaContractViolation::UnexpectedFormat {
            expected: EXPECTED_FORMAT,
            actual: def.format.clone(),
        });
    }
    if def.channels != EXPECTED_CHANNELS {
        violations.push(AlsaContractViolation::UnexpectedChannels {
            expected: EXPECTED_CHANNELS,
            actual: def.channels,
        });
    }
    if def.rate_policy != RatePolicy::Unchanged {
        violations.push(AlsaContractViolation::UnexpectedRatePolicy {
            actual: def.rate_policy.clone(),
        });
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_definition_parses_and_is_compliant() {
        let def = parse_slave_block(CANONICAL_ASOUND_CONF).expect("canonical config must parse");
        assert_eq!(def.slave_pcm, "hw:Loopback,1,0");
        assert_eq!(def.format, "S32_LE");
        assert_eq!(def.channels, 2);
        assert_eq!(def.rate_policy, RatePolicy::Unchanged);
        assert_eq!(validate_alsa_contract(&def), Ok(()));
    }

    #[test]
    fn s16_producer_format_is_rejected() {
        let conf = r#"pcm.picorecdsp {
            type plug
            slave {
                pcm "hw:Loopback,1,0"
                format S16_LE
                channels 2
                rate unchanged
            }
        }"#;
        let def = parse_slave_block(conf).unwrap();
        let violations = validate_alsa_contract(&def).unwrap_err();
        assert_eq!(
            violations,
            vec![AlsaContractViolation::UnexpectedFormat {
                expected: "S32_LE",
                actual: "S16_LE".into()
            }]
        );
    }

    #[test]
    fn mono_channel_count_is_rejected() {
        let conf = r#"pcm.picorecdsp {
            slave {
                pcm "hw:Loopback,1,0"
                format S32_LE
                channels 1
                rate unchanged
            }
        }"#;
        let def = parse_slave_block(conf).unwrap();
        let violations = validate_alsa_contract(&def).unwrap_err();
        assert_eq!(
            violations,
            vec![AlsaContractViolation::UnexpectedChannels {
                expected: 2,
                actual: 1
            }]
        );
    }

    #[test]
    fn fixed_rate_instead_of_unchanged_is_rejected() {
        let conf = r#"pcm.picorecdsp {
            slave {
                pcm "hw:Loopback,1,0"
                format S32_LE
                channels 2
                rate 48000
            }
        }"#;
        let def = parse_slave_block(conf).unwrap();
        let violations = validate_alsa_contract(&def).unwrap_err();
        assert_eq!(
            violations,
            vec![AlsaContractViolation::UnexpectedRatePolicy {
                actual: RatePolicy::Other("48000".into())
            }]
        );
    }

    #[test]
    fn multiple_violations_are_all_reported() {
        let conf = r#"pcm.picorecdsp {
            slave {
                pcm "hw:Loopback,1,0"
                format S16_LE
                channels 1
                rate 44100
            }
        }"#;
        let def = parse_slave_block(conf).unwrap();
        let violations = validate_alsa_contract(&def).unwrap_err();
        assert_eq!(violations.len(), 3);
    }

    #[test]
    fn missing_field_is_a_parse_error_not_a_silent_default() {
        let conf = r#"pcm.picorecdsp {
            slave {
                pcm "hw:Loopback,1,0"
                channels 2
                rate unchanged
            }
        }"#;
        let err = parse_slave_block(conf).unwrap_err();
        assert_eq!(err.missing_field, "slave.format");
    }
}
