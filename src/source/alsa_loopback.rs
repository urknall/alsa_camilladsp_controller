//! ALSA ingress contract (roadmap `piCoreCDSP_v2_Roadmap.md` §6).
//!
//! Defines the canonical `pcm.camilladsp` `plug` definition that every producer must
//! go through on its way to `snd-aloop`, and a validator that checks a parsed
//! definition against the contract's invariants: `format = S32_LE`,
//! `channels = 2`, `samplerate = unchanged`.
//!
//! This module only *describes and validates* the contract. Writing the definition to
//! `/etc/asound.conf` (or equivalent) is an installer concern (roadmap Gate 11), not a
//! runtime Rust concern.

/// The canonical `pcm.camilladsp` ALSA `plug` definition (roadmap §6), targeting the
/// first `snd-aloop` playback subdevice as the shared ingress point for every
/// producer.
pub const CANONICAL_ASOUND_CONF: &str = r#"pcm.camilladsp {
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

/// Parses the `slave { ... }` block of a `pcm.camilladsp` `plug` definition.
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
        let conf = r#"pcm.camilladsp {
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
        let conf = r#"pcm.camilladsp {
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
        let conf = r#"pcm.camilladsp {
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
        let conf = r#"pcm.camilladsp {
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
        let conf = r#"pcm.camilladsp {
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

// ── AlsaLoopbackObserver (Linux only) ────────────────────────────────────────

/// A real [`SourceObserver`] that reads loopback state from
/// `/proc/asound/<card>/pcm<device>p/sub<subdevice>/`.
///
/// # How it works
///
/// `snd-aloop` exposes the PCM state of each subdevice under
/// `/proc/asound/`.  This observer reads two files on every `snapshot()` call:
///
/// * `status` — "closed" when no producer is open; any other value
///   (e.g. "state: RUNNING") means a producer is active.
/// * `hw_params` — parsed for the `rate:` line to extract the sample rate.
///
/// `next_trigger()` polls these files every `poll_interval` and returns when
/// the active/inactive state changes.
///
/// # Default paths
///
/// | Setting       | Default          | Meaning                              |
/// |---------------|------------------|--------------------------------------|
/// | `card`        | `"Loopback"`     | ALSA card name under `/proc/asound/` |
/// | `device`      | `1`              | PCM device index (playback side)     |
/// | `subdevice`   | `0`              | Subdevice index                      |
/// | `poll_interval` | 50 ms          | How often to re-read for triggers    |
///
/// The defaults correspond to the piCoreCDSP canonical config where the
/// producer opens `hw:Loopback,1,0` (device 1, subdevice 0) and CamillaDSP
/// reads from `hw:Loopback,0,0`.
///
/// # Removal criterion
///
/// Registered in `upstream/capabilities.yml` under `native_aloop_rate_following`.
/// Delete when CamillaDSP natively detects loopback active/rate/stop.
#[cfg(target_os = "linux")]
pub struct AlsaLoopbackObserver {
    status_path: std::path::PathBuf,
    hw_params_path: std::path::PathBuf,
    poll_interval: std::time::Duration,
    generation: u64,
    last_active: bool,
}

#[cfg(target_os = "linux")]
impl AlsaLoopbackObserver {
    /// Create a new observer with default paths for the canonical piCoreCDSP setup.
    pub fn new_default() -> Self {
        Self::new("Loopback", 1, 0, std::time::Duration::from_millis(50))
    }

    /// Create a new observer with explicit card name, device, subdevice, and poll interval.
    pub fn new(
        card: &str,
        device: u32,
        subdevice: u32,
        poll_interval: std::time::Duration,
    ) -> Self {
        let base = std::path::PathBuf::from(format!(
            "/proc/asound/{}/pcm{}p/sub{}",
            card, device, subdevice
        ));
        Self {
            status_path: base.join("status"),
            hw_params_path: base.join("hw_params"),
            poll_interval,
            generation: 0,
            last_active: false,
        }
    }

    fn read_snapshot_inner(&self) -> crate::source::SourceSnapshot {
        use crate::source::{SourceSnapshot, SourceState};

        let status = std::fs::read_to_string(&self.status_path).unwrap_or_default();
        let active = !status.trim().is_empty() && status.trim() != "closed";

        let sample_rate = if active {
            std::fs::read_to_string(&self.hw_params_path)
                .ok()
                .and_then(|hw| parse_proc_rate(&hw))
        } else {
            None
        };

        let state = if active {
            if let Some(r) = sample_rate {
                SourceState::Active { sample_rate: r }
            } else {
                // Status says active but we couldn't read the rate yet — still
                // report Inactive so the reconciler waits for the rate to settle.
                SourceState::Inactive
            }
        } else {
            SourceState::Inactive
        };

        SourceSnapshot {
            state,
            sample_rate,
            format: None,
            channels: None,
            generation: self.generation,
        }
    }
}

/// Parse `rate: 44100 (44100/1)` from a `/proc/asound/.../hw_params` file.
#[cfg(target_os = "linux")]
fn parse_proc_rate(hw_params: &str) -> Option<u32> {
    for line in hw_params.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("rate:") {
            // rest is something like " 44100 (44100/1)"
            let first_token = rest.split_whitespace().next()?;
            return first_token.parse::<u32>().ok();
        }
    }
    None
}

#[cfg(target_os = "linux")]
#[async_trait::async_trait]
impl crate::source::observer::SourceObserver for AlsaLoopbackObserver {
    async fn snapshot(
        &self,
    ) -> Result<crate::source::SourceSnapshot, crate::error::PicorecdspError> {
        Ok(self.read_snapshot_inner())
    }

    async fn next_trigger(&mut self) -> Result<(), crate::error::PicorecdspError> {
        loop {
            tokio::time::sleep(self.poll_interval).await;
            let snap = self.read_snapshot_inner();
            let now_active = snap.state.is_active();
            if now_active != self.last_active {
                if now_active {
                    self.generation = self.generation.wrapping_add(1);
                }
                self.last_active = now_active;
                return Ok(());
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod proc_observer_tests {
    use super::*;

    #[test]
    fn parse_proc_rate_extracts_rate() {
        let hw_params = "access: MMAP_INTERLEAVED\nformat: S32_LE\nsubformat: STD\nchannels: 2\nrate: 44100 (44100/1)\nperiod_size: 11025\nbuffer_size: 44100\n";
        assert_eq!(parse_proc_rate(hw_params), Some(44_100));
    }

    #[test]
    fn parse_proc_rate_various_rates() {
        for rate in &[44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000] {
            let hw_params = format!("rate: {rate} ({rate}/1)\n");
            assert_eq!(parse_proc_rate(&hw_params), Some(*rate));
        }
    }

    #[test]
    fn parse_proc_rate_returns_none_for_empty() {
        assert_eq!(parse_proc_rate(""), None);
        assert_eq!(parse_proc_rate("access: MMAP_INTERLEAVED\n"), None);
    }
}
