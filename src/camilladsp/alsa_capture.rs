use crate::core::errors::{app_error, AppResult};
use crate::core::logging::{log, LogLevel};
use crate::core::config::{DeviceSnapshot, WaveFormat};
use alsa::ctl::ElemIface;
use alsa::hctl::HCtl;

const LOOPBACK_ACTIVE: &str = "PCM Slave Active";
const LOOPBACK_CHANNELS: &str = "PCM Slave Channels";
const LOOPBACK_FORMAT: &str = "PCM Slave Format";
const LOOPBACK_RATE: &str = "PCM Slave Rate";
const GADGET_CAPTURE_RATE: &str = "Capture Rate";

// ─── Listener trait ────────────────────────────────────────────────────────

/// Abstraction over the ALSA loopback control interface, used by the controller.
///
/// Defining a trait enables mock implementations for unit-testing the
/// controller state machine without physical ALSA hardware.
pub trait DeviceListener {
    fn wait_for_event(&self, timeout_ms: u32) -> AppResult<bool>;
    fn handle_events(&self) -> AppResult<()>;
    fn read_snapshot(&self) -> AppResult<DeviceSnapshot>;
}

/// Non-blocking listener for the ALSA `snd-aloop` (or USB gadget) HCTL interface.
///
/// Wraps an open `HCtl` handle and provides snapshot reads and event polling.
/// The open handle matches `alsa-python`'s `HControl(card, NONBLOCK)` semantics.
pub struct AlsaLoopbackListener {
    hctl: HCtl,
    device: u32,
    subdevice: u32,
    log_level: LogLevel,
}

impl AlsaLoopbackListener {
    /// Open the ALSA control device and verify the expected snd-aloop controls
    /// are present. Returns an error immediately if any required control is missing,
    /// making kernel/ABI mismatches visible at install time via `--probe`.
    ///
    /// `device_name` follows the same parsing rules as the Python controller:
    /// - `"hw:Loopback,0,0"` → card=`hw:Loopback`, device=0, subdevice=0
    /// - `"hw:Loopback,0"` → card=`hw:Loopback`, device=0, subdevice=0
    pub fn new(device_name: &str, log_level: LogLevel) -> AppResult<Self> {
        let parts: Vec<&str> = device_name.split(',').collect();
        let card = parts
            .first()
            .copied()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| app_error("empty ALSA control device"))?;
        let device = if parts.len() >= 2 {
            parts[1]
                .parse::<u32>()
                .map_err(|_| app_error(format!("invalid ALSA device number in {device_name}")))?
        } else {
            0
        };
        let subdevice = if parts.len() >= 3 {
            parts[2]
                .parse::<u32>()
                .map_err(|_| app_error(format!("invalid ALSA subdevice number in {device_name}")))?
        } else {
            0
        };

        // Non-blocking HCTL matches alsa-python's HControl(card, NONBLOCK).
        let hctl = HCtl::new(card, true)?;
        hctl.load()?;

        let listener = Self {
            hctl,
            device,
            subdevice,
            log_level,
        };

        // Fail early if this is not the snd-aloop control device expected by
        // piCoreDSP. This is more useful than silently running with no controls.
        let snapshot = listener.read_snapshot()?;
        log(
            LogLevel::Debug,
            log_level,
            format!(
                "Initial ALSA snapshot: active={}, {}",
                snapshot.active, snapshot.wave
            ),
        );
        Ok(listener)
    }

    /// Block until the next HCTL event or `timeout_ms` milliseconds elapse.
    /// Returns `true` if an event is available.
    pub fn wait_for_event(&self, timeout_ms: u32) -> AppResult<bool> {
        Ok(self.hctl.wait(Some(timeout_ms))?)
    }

    /// Drain pending kernel HCTL events so subsequent reads reflect the latest
    /// control values.
    pub fn handle_events(&self) -> AppResult<()> {
        self.hctl.handle_events()?;
        Ok(())
    }

    /// Read the current state of all loopback (or gadget) controls for this
    /// device/subdevice pair and return a consistent snapshot.
    pub fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
        let mut loopback_active: Option<bool> = None;
        let mut channels: Option<u32> = None;
        let mut raw_format: Option<i32> = None;
        let mut loopback_rate: Option<u32> = None;
        let mut gadget_rate: Option<u32> = None;

        for elem in self.hctl.elem_iter() {
            let id = elem.get_id()?;
            if id.get_interface() != ElemIface::PCM
                || id.get_device() != self.device
                || id.get_subdevice() != self.subdevice
            {
                continue;
            }

            let name = id.get_name()?.to_owned();
            match name.as_str() {
                LOOPBACK_ACTIVE => {
                    let value = elem.read()?;
                    loopback_active = value.get_boolean(0);
                }
                LOOPBACK_CHANNELS => {
                    let value = elem.read()?;
                    channels = value.get_integer(0).and_then(nonneg_u32);
                }
                LOOPBACK_FORMAT => {
                    let value = elem.read()?;
                    raw_format = value.get_integer(0);
                }
                LOOPBACK_RATE => {
                    let value = elem.read()?;
                    loopback_rate = value.get_integer(0).and_then(nonneg_u32);
                }
                GADGET_CAPTURE_RATE => {
                    let value = elem.read()?;
                    gadget_rate = value.get_integer(0).and_then(nonneg_u32);
                }
                _ => {}
            }
        }

        // Match the Python controller's USB-gadget behavior: when "Capture Rate"
        // is present it is authoritative; format and channels are unknown.
        if let Some(rate) = gadget_rate {
            return Ok(DeviceSnapshot {
                active: rate > 0,
                wave: WaveFormat {
                    sample_rate: Some(rate),
                    sample_format: None,
                    channels: None,
                },
            });
        }

        // For snd-aloop all four controls must exist. Require them so a
        // kernel/ABI mismatch is caught at install time via --probe.
        let active = loopback_active.ok_or_else(|| {
            app_error(format!(
                "ALSA control '{LOOPBACK_ACTIVE}' not found for device {},{}",
                self.device, self.subdevice
            ))
        })?;
        let rate = loopback_rate.ok_or_else(|| {
            app_error(format!(
                "ALSA control '{LOOPBACK_RATE}' not found for device {},{}",
                self.device, self.subdevice
            ))
        })?;
        let channels = channels.ok_or_else(|| {
            app_error(format!(
                "ALSA control '{LOOPBACK_CHANNELS}' not found for device {},{}",
                self.device, self.subdevice
            ))
        })?;
        let raw_format = raw_format.ok_or_else(|| {
            app_error(format!(
                "ALSA control '{LOOPBACK_FORMAT}' not found for device {},{}",
                self.device, self.subdevice
            ))
        })?;

        let sample_format = alsa_format_to_camilladsp(raw_format)?;
        if sample_format.is_none() {
            log(
                LogLevel::Warning,
                self.log_level,
                format!(
                    "ALSA capture format {raw_format} is within the valid snd_pcm_format_t \
                     range but has no CamillaDSP mapping; format will not be updated in the \
                     active config — CamillaDSP may fail to start if the config specifies a \
                     different format"
                ),
            );
        }

        Ok(DeviceSnapshot {
            active,
            wave: WaveFormat {
                sample_rate: Some(rate),
                sample_format: sample_format.map(str::to_owned),
                channels: Some(channels),
            },
        })
    }
}

/// Return `Some(value as u32)` for non-negative `i32` values, `None` otherwise.
fn nonneg_u32(value: i32) -> Option<u32> {
    (value >= 0).then_some(value as u32)
}

/// Map a Linux `snd_pcm_format_t` integer to the corresponding CamillaDSP ALSA
/// config format name string.
///
/// | ALSA value | `snd_pcm_format_t` name        | CamillaDSP ALSA name |
/// |-----------:|--------------------------------|----------------------|
/// |          2 | `SND_PCM_FORMAT_S16_LE`        | `S16_LE`             |
/// |          6 | `SND_PCM_FORMAT_S24_LE`        | `S24_4_LE`           |
/// |         10 | `SND_PCM_FORMAT_S32_LE`        | `S32_LE`             |
/// |         14 | `SND_PCM_FORMAT_FLOAT_LE`      | `F32_LE`             |
/// |         16 | `SND_PCM_FORMAT_FLOAT64_LE`    | `F64_LE`             |
/// |         32 | `SND_PCM_FORMAT_S24_3LE`       | `S24_3_LE`           |
///
/// Note on value 6: `SND_PCM_FORMAT_S24_LE` is a 24-bit sample in a 4-byte
/// container.  The correct ALSA device-config name in CamillaDSP 4.1 is
/// `S24_4_LE`; CamillaDSP maps this internally to `BinarySampleFormat::S24_4_RJ_LE`.
/// Using `S24_4_RJ_LE` directly in the ALSA capture/playback block is invalid
/// and produces a schema error at runtime.
///
/// Values within the valid `snd_pcm_format_t` range but without a CamillaDSP
/// counterpart return `Ok(None)`. Genuinely unknown values return an error.
pub fn alsa_format_to_camilladsp(value: i32) -> AppResult<Option<&'static str>> {
    let mapped = match value {
        2 => Some("S16_LE"),
        6 => Some("S24_4_LE"),
        10 => Some("S32_LE"),
        14 => Some("F32_LE"),
        16 => Some("F64_LE"),
        32 => Some("S24_3_LE"),
        // Within snd_pcm_format_t range but not mapped to a CamillaDSP format.
        0..=28 | 31..=52 => None,
        _ => {
            return Err(app_error(format!(
                "unknown ALSA sample-format enum value {value}"
            )))
        }
    };
    Ok(mapped)
}

impl DeviceListener for AlsaLoopbackListener {
    fn wait_for_event(&self, timeout_ms: u32) -> AppResult<bool> {
        AlsaLoopbackListener::wait_for_event(self, timeout_ms)
    }
    fn handle_events(&self) -> AppResult<()> {
        AlsaLoopbackListener::handle_events(self)
    }
    fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
        AlsaLoopbackListener::read_snapshot(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mapping_produces_camilladsp_alsa_config_names() {
        assert_eq!(alsa_format_to_camilladsp(2).unwrap(), Some("S16_LE"));
        assert_eq!(alsa_format_to_camilladsp(6).unwrap(), Some("S24_4_LE"));
        assert_eq!(alsa_format_to_camilladsp(10).unwrap(), Some("S32_LE"));
        assert_eq!(alsa_format_to_camilladsp(14).unwrap(), Some("F32_LE"));
        assert_eq!(alsa_format_to_camilladsp(16).unwrap(), Some("F64_LE"));
        assert_eq!(alsa_format_to_camilladsp(32).unwrap(), Some("S24_3_LE"));
        // Values in range but unmapped.
        assert_eq!(alsa_format_to_camilladsp(0).unwrap(), None);
        assert_eq!(alsa_format_to_camilladsp(31).unwrap(), None); // SND_PCM_FORMAT_SPECIAL
                                                                  // Out-of-range values are errors.
        assert!(alsa_format_to_camilladsp(99).is_err());
        assert!(alsa_format_to_camilladsp(-1).is_err());
    }
}
