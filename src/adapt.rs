use crate::error::{app_error, AppResult};
use crate::wave::WaveFormat;
use serde_yaml_ng::{Mapping, Value as YamlValue};
use std::fs;
use std::path::Path;

// ─── YAML helpers ──────────────────────────────────────────────────────────

pub fn yaml_key(name: &str) -> YamlValue {
    YamlValue::String(name.to_owned())
}

pub fn mapping_mut<'a>(value: &'a mut YamlValue, context: &str) -> AppResult<&'a mut Mapping> {
    value
        .as_mapping_mut()
        .ok_or_else(|| app_error(format!("{context} must be a YAML mapping")))
}

pub fn mapping<'a>(value: &'a YamlValue, context: &str) -> AppResult<&'a Mapping> {
    value
        .as_mapping()
        .ok_or_else(|| app_error(format!("{context} must be a YAML mapping")))
}

pub fn yaml_u32(value: &YamlValue) -> Option<u32> {
    value.as_u64().and_then(|v| u32::try_from(v).ok())
}

// ─── Config adaptation ─────────────────────────────────────────────────────

/// Adapt a CamillaDSP YAML config for the current wave format and return the
/// updated YAML string.
///
/// The path is intentionally **re-read on every call**. Because the installer
/// passes `active_config.yml` (a symlink managed by CamillaGUI), re-reading
/// means a config change made in the GUI is picked up on the very next
/// adaptation without a controller restart. This is the key piCoreDSP
/// extension over upstream `AdaptConfig`, which caches the file at startup.
///
/// Adaptation rules mirror Python `AdaptConfig._change_*` exactly:
///
/// * **samplerate** — updated in `devices.samplerate` unless a resampler is
///   present, in which case `devices.capture_samplerate` is set instead (and a
///   synchronous 1:1 resampler is removed for this runtime copy).
/// * **format** — updated in `devices.capture.format` only when that key
///   already exists with a non-null value (automatic format is left automatic).
/// * **channels** — validated only; changing channel count is not supported.
pub fn adapt_config(path: &Path, wave: &WaveFormat) -> AppResult<String> {
    let raw = fs::read_to_string(path)
        .map_err(|err| app_error(format!("unable to read config {}: {err}", path.display())))?;
    let mut root: YamlValue = serde_yaml_ng::from_str(&raw)?;

    let root_map = mapping_mut(&mut root, "config root")?;
    let devices_value = root_map
        .get_mut(&yaml_key("devices"))
        .ok_or_else(|| app_error("config has no 'devices' section"))?;
    let devices = mapping_mut(devices_value, "devices")?;

    if let Some(rate) = wave.sample_rate {
        let resampler_key = yaml_key("resampler");
        let has_resampler = devices
            .get(&resampler_key)
            .map(|v| !v.is_null())
            .unwrap_or(false);

        if !has_resampler {
            devices.insert(yaml_key("samplerate"), YamlValue::from(rate as u64));
        } else {
            // Resampler is present: set capture_samplerate and potentially
            // remove a synchronous 1:1 resampler, matching Python AdaptConfig.
            let resampler = devices
                .get(&resampler_key)
                .ok_or_else(|| app_error("devices.resampler disappeared during adaptation"))?;
            let resampler_map = mapping(resampler, "devices.resampler")?;
            let resampler_type = resampler_map
                .get(&yaml_key("type"))
                .and_then(YamlValue::as_str)
                .ok_or_else(|| app_error("devices.resampler.type must be a string"))?
                .to_owned();

            let configured_rate = devices.get(&yaml_key("samplerate")).and_then(yaml_u32);

            devices.insert(
                yaml_key("capture_samplerate"),
                YamlValue::from(rate as u64),
            );

            if resampler_type == "Synchronous" && configured_rate == Some(rate) {
                devices.insert(resampler_key, YamlValue::Null);
            }
        }
    }

    let capture_value = devices
        .get_mut(&yaml_key("capture"))
        .ok_or_else(|| app_error("config has no 'devices.capture' section"))?;
    let capture = mapping_mut(capture_value, "devices.capture")?;

    // Only update `format` when the config explicitly sets it (non-null).
    // Configs that omit `format` use CamillaDSP's automatic detection.
    if let Some(format) = wave.sample_format.as_deref() {
        let format_key = yaml_key("format");
        if capture
            .get(&format_key)
            .map(|value| !value.is_null())
            .unwrap_or(false)
        {
            capture.insert(format_key, YamlValue::String(format.to_owned()));
        }
    }

    // Channel count changes are not supported; validate equality only.
    if let Some(channels) = wave.channels {
        let configured = capture
            .get(&yaml_key("channels"))
            .and_then(yaml_u32)
            .ok_or_else(|| app_error("devices.capture.channels is missing or invalid"))?;
        if configured != channels {
            return Err(app_error(format!(
                "changing capture channels is not implemented \
                 (config={configured}, stream={channels})"
            )));
        }
    }

    Ok(serde_yaml_ng::to_string(&root)?)
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "picoredsp-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Generate a minimal CamillaDSP config for test use.
    fn base_config(playback: &str, format: Option<&str>) -> String {
        let format_line = format
            .map(|fmt| format!("    format: {fmt}\n"))
            .unwrap_or_default();
        format!(
            "devices:\n  samplerate: 44100\n  chunksize: 2048\n  \
             capture:\n    type: Alsa\n    channels: 2\n    \
             device: \"hw:Loopback,0,0\"\n{format_line}  \
             playback:\n    type: Alsa\n    channels: 2\n    \
             device: \"{playback}\"\nfilters: {{}}\nmixers: {{}}\n\
             pipeline: []\nprocessors: {{}}\n"
        )
    }

    #[test]
    fn adapt_updates_rate_and_explicit_format_keeps_playback() {
        let dir = test_dir("adapt");
        let config = dir.join("config.yml");
        fs::write(&config, base_config("hw:99,0", Some("S16_LE"))).unwrap();
        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: Some("S32_LE".to_owned()),
            channels: Some(2),
        };
        let adapted = adapt_config(&config, &wave).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();
        assert_eq!(parsed["devices"]["samplerate"].as_u64(), Some(48000));
        assert_eq!(
            parsed["devices"]["capture"]["format"].as_str(),
            Some("S32_LE")
        );
        assert_eq!(
            parsed["devices"]["playback"]["device"].as_str(),
            Some("hw:99,0")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn automatic_capture_format_is_not_touched() {
        let dir = test_dir("autoformat");
        let config = dir.join("config.yml");
        // Config has no `format:` key → automatic mode.
        fs::write(&config, base_config("null", None)).unwrap();
        let wave = WaveFormat {
            sample_rate: Some(96000),
            sample_format: Some("S32_LE".to_owned()),
            channels: Some(2),
        };
        let adapted = adapt_config(&config, &wave).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();
        // The format key must remain absent.
        assert!(parsed["devices"]["capture"].get("format").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn active_symlink_is_reread_on_every_adaptation() {
        let dir = test_dir("symlink");
        let first = dir.join("first.yml");
        let second = dir.join("second.yml");
        let active = dir.join("active.yml");
        fs::write(&first, base_config("null", Some("S16_LE"))).unwrap();
        fs::write(&second, base_config("hw:99,0", Some("S16_LE"))).unwrap();
        symlink(&first, &active).unwrap();

        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: Some("S32_LE".to_owned()),
            channels: Some(2),
        };

        let first_adapted = adapt_config(&active, &wave).unwrap();
        let first_parsed: YamlValue = serde_yaml_ng::from_str(&first_adapted).unwrap();
        assert_eq!(
            first_parsed["devices"]["playback"]["device"].as_str(),
            Some("null")
        );

        // Atomically retarget the symlink (as CamillaGUI does via on_set_active_config).
        fs::remove_file(&active).unwrap();
        symlink(&second, &active).unwrap();

        let second_adapted = adapt_config(&active, &wave).unwrap();
        let second_parsed: YamlValue = serde_yaml_ng::from_str(&second_adapted).unwrap();
        assert_eq!(
            second_parsed["devices"]["playback"]["device"].as_str(),
            Some("hw:99,0")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn synchronous_one_to_one_resampler_is_disabled_for_runtime_copy() {
        let dir = test_dir("resampler");
        let config = dir.join("config.yml");
        fs::write(
            &config,
            "devices:\n  samplerate: 48000\n  resampler:\n    type: Synchronous\n  \
             capture:\n    type: Alsa\n    channels: 2\n    device: \"hw:Loopback,0,0\"\n  \
             playback:\n    type: Alsa\n    channels: 2\n    device: \"null\"\n\
             filters: {}\nmixers: {}\npipeline: []\nprocessors: {}\n",
        )
        .unwrap();
        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: None,
            channels: Some(2),
        };
        let adapted = adapt_config(&config, &wave).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();
        assert!(parsed["devices"]["resampler"].is_null());
        assert_eq!(parsed["devices"]["capture_samplerate"].as_u64(), Some(48000));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_resampler_missing_type_is_rejected() {
        let dir = test_dir("bad-resampler");
        let config = dir.join("config.yml");
        // `resampler: {}` has no `type` key → must error.
        fs::write(
            &config,
            "devices:\n  samplerate: 48000\n  resampler: {}\n  \
             capture:\n    type: Alsa\n    channels: 2\n    device: \"hw:Loopback,0,0\"\n  \
             playback:\n    type: Alsa\n    channels: 2\n    device: \"null\"\n\
             filters: {}\nmixers: {}\npipeline: []\nprocessors: {}\n",
        )
        .unwrap();
        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: None,
            channels: Some(2),
        };
        assert!(adapt_config(&config, &wave).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn channel_count_change_is_rejected() {
        let dir = test_dir("channels");
        let config = dir.join("config.yml");
        fs::write(&config, base_config("null", Some("S16_LE"))).unwrap();
        // Config has 2 channels; stream claims 6 → must error.
        let wave = WaveFormat {
            sample_rate: Some(44100),
            sample_format: Some("S16_LE".to_owned()),
            channels: Some(6),
        };
        assert!(adapt_config(&config, &wave).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn yaml_helpers_accept_normal_camilladsp_config() {
        let value: YamlValue = serde_yaml_ng::from_str(&base_config("null", None)).unwrap();
        let root = mapping(&value, "root").unwrap();
        assert!(root.contains_key(&yaml_key("devices")));
    }
}
