use crate::core::config::WaveFormat;
use crate::core::errors::{app_error, AppResult};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::{Mapping, Value as YamlValue};
use std::fs;
use std::path::{Component, Path, PathBuf};

const CAMILLA_STATE_CHANNELS: usize = 5;
const ALOOP_CAPTURE_DEVICE: &str = "hw:Loopback,0,0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeBackend {
    Aloop,
    Ioplug,
}

// ─── Statefile types ───────────────────────────────────────────────────────

/// Typed representation of a CamillaDSP statefile, used for both reading an
/// existing statefile and writing a newly generated one.
///
/// Using fixed-length arrays `[bool; 5]` and `[f32; 5]` means serde
/// automatically enforces both element type and exact length (5) without any
/// manual validation loop.  `serde_yaml_ng::to_string` handles all necessary
/// YAML quoting for `config_path`, including filenames that contain spaces,
/// colons, brackets, or other YAML-significant characters.
///
/// `config_path` is `Option<String>` because CamillaDSP 4.1.3 writes `null`
/// when it is started with `--no_config` (the controller's normal boot path).
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StateFile {
    pub config_path: Option<String>,
    pub mute: [bool; 5],
    pub volume: [f32; 5],
}

/// Create a CamillaDSP statefile YAML string.
///
/// * **First install** (`existing_state_path = None`): `config_path` is taken
///   from the argument; `mute` and `volume` are initialised to safe defaults
///   (`false` / `0.0`).
/// * **Reinstall** (`existing_state_path = Some(path)`): `mute` and `volume`
///   are preserved exactly from the existing statefile; `config_path` comes
///   from the argument.  Any error reading or parsing the existing statefile
///   is propagated immediately — no silent fallback to defaults is ever
///   applied when `--existing-state` is supplied.
pub fn make_statefile(config_path: &str, existing_state_path: Option<&Path>) -> AppResult<String> {
    let (mute, volume) = match existing_state_path {
        Some(path) => {
            let raw = fs::read_to_string(path).map_err(|err| {
                app_error(format!(
                    "unable to read existing statefile {}: {err}",
                    path.display()
                ))
            })?;
            let sf: StateFile = serde_yaml_ng::from_str(&raw)
                .map_err(|err| app_error(format!("invalid statefile {}: {err}", path.display())))?;
            if sf.volume.iter().any(|value| !value.is_finite()) {
                return Err(app_error(format!(
                    "invalid statefile {}: volume values must be finite",
                    path.display()
                )));
            }
            (sf.mute, sf.volume)
        }
        None => ([false; 5], [0.0_f32; 5]),
    };

    let sf = StateFile {
        config_path: Some(config_path.to_owned()),
        mute,
        volume,
    };
    Ok(serde_yaml_ng::to_string(&sf)?)
}

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

// ─── Path helpers ──────────────────────────────────────────────────────────

/// Resolve `..` and `.` components without requiring the path to exist.
///
/// Used to absolutize FIR coefficient filenames when `canonicalize()` would
/// fail because a test fixture or missing coefficient file is not on disk.
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if matches!(result.components().next_back(), Some(Component::Normal(_))) {
                    result.pop();
                }
            }
            Component::CurDir => {}
            c => result.push(c),
        }
    }
    result
}

/// Walk the `filters` section of a CamillaDSP config and convert any relative
/// `filename` fields inside `Conv` filters (type `Raw` or `Wav`) to absolute
/// paths anchored at `config_dir`.
///
/// This mirrors CamillaGUI's `make_config_filter_paths_absolute()`, which it
/// applies before every `SetConfig` call.  Without this conversion, relative
/// paths in a config that is sent over the WebSocket are resolved against
/// CamillaDSP's process working directory rather than the config file's
/// directory, which causes coefficient loads to fail.
fn make_filter_paths_absolute(root: &mut YamlValue, config_dir: &Path) {
    let Some(filters) = root.get_mut("filters") else {
        return;
    };
    let Some(filters_map) = filters.as_mapping_mut() else {
        return;
    };

    for (_name, filter) in filters_map.iter_mut() {
        let Some(filter_map) = filter.as_mapping_mut() else {
            continue;
        };

        // Only process Conv filters.
        let is_conv = filter_map
            .get(yaml_key("type"))
            .and_then(YamlValue::as_str)
            .map(|t| t == "Conv")
            .unwrap_or(false);
        if !is_conv {
            continue;
        }

        let Some(params) = filter_map.get_mut(yaml_key("parameters")) else {
            continue;
        };
        let Some(params_map) = params.as_mapping_mut() else {
            continue;
        };

        // Only Raw/Wav types have a file-based coefficient.
        match params_map.get(yaml_key("type")).and_then(YamlValue::as_str) {
            Some("Raw") | Some("Wav") => {}
            _ => continue,
        }

        if let Some(filename_val) = params_map.get_mut(yaml_key("filename")) {
            if let Some(name_str) = filename_val.as_str() {
                let p = Path::new(name_str);
                if p.is_relative() {
                    let abs = config_dir.join(p);
                    let resolved = abs.canonicalize().unwrap_or_else(|_| normalize_path(&abs));
                    *filename_val = YamlValue::String(resolved.to_string_lossy().into_owned());
                }
            }
        }
    }
}

fn runtime_managed_capture_field(name: &str) -> bool {
    matches!(
        name,
        "type" | "device" | "format" | "channels" | "stop_on_inactive"
    )
}

fn existing_capture_mapping(devices: &Mapping) -> AppResult<Option<&Mapping>> {
    devices
        .get(yaml_key("capture"))
        .map(|value| mapping(value, "devices.capture"))
        .transpose()
}

fn existing_capture_format(existing_capture: Option<&Mapping>) -> AppResult<Option<String>> {
    let Some(capture) = existing_capture else {
        return Ok(None);
    };
    let Some(value) = capture.get(yaml_key("format")) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(str::to_owned)
        .map(Some)
        .ok_or_else(|| app_error("devices.capture.format must be a string"))
}

fn existing_capture_channels(existing_capture: Option<&Mapping>) -> AppResult<Option<u32>> {
    let Some(capture) = existing_capture else {
        return Ok(None);
    };
    let Some(value) = capture.get(yaml_key("channels")) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    yaml_u32(value)
        .map(Some)
        .ok_or_else(|| app_error("devices.capture.channels is missing or invalid"))
}

fn resolve_capture_channels(
    existing_capture: Option<&Mapping>,
    wave: &WaveFormat,
) -> AppResult<u32> {
    let configured = existing_capture_channels(existing_capture)?;
    if let (Some(configured), Some(channels)) = (configured, wave.channels) {
        if configured != channels {
            return Err(app_error(format!(
                "changing capture channels is not implemented \
                 (config={configured}, stream={channels})"
            )));
        }
    }
    Ok(wave.channels.or(configured).unwrap_or(2))
}

fn base_runtime_capture(existing_capture: Option<&Mapping>) -> Mapping {
    let mut capture = Mapping::new();
    let Some(existing_capture) = existing_capture else {
        return capture;
    };

    for (key, value) in existing_capture {
        let preserve = key
            .as_str()
            .map(|name| !runtime_managed_capture_field(name))
            .unwrap_or(true);
        if preserve {
            capture.insert(key.clone(), value.clone());
        }
    }

    capture
}

/// Build the non-runtime-managed portion of a CamillaDSP `Stdin` capture block.
///
/// CamillaDSP device variants use strict schemas (`deny_unknown_fields`).  A
/// baseline config saved by CamillaGUI may contain ALSA-only capture keys such
/// as `link_mute_control` and `link_volume_control`, even when their values are
/// `null`.  Merely changing `type: Alsa` to `type: Stdin` therefore produces an
/// invalid config.  Only carry fields that are valid for the Stdin device; the
/// runtime-managed `channels` and `format` fields are injected afterwards.
fn base_stdin_capture(existing_capture: Option<&Mapping>) -> Mapping {
    let mut capture = Mapping::new();
    let Some(existing_capture) = existing_capture else {
        return capture;
    };

    for (key, value) in existing_capture {
        let preserve = key
            .as_str()
            .map(|name| {
                matches!(
                    name,
                    "labels" | "extra_samples" | "skip_bytes" | "read_bytes"
                )
            })
            .unwrap_or(false);
        if preserve {
            capture.insert(key.clone(), value.clone());
        }
    }

    capture
}

fn build_runtime_capture(
    existing_capture: Option<&Mapping>,
    wave: &WaveFormat,
    backend: RuntimeBackend,
) -> AppResult<Mapping> {
    let channels = resolve_capture_channels(existing_capture, wave)?;
    let explicit_format = existing_capture_format(existing_capture)?;

    let capture = match backend {
        RuntimeBackend::Aloop => {
            let mut capture = base_runtime_capture(existing_capture);
            capture.insert(yaml_key("type"), YamlValue::String("Alsa".to_owned()));
            capture.insert(yaml_key("channels"), YamlValue::from(channels as u64));
            capture.insert(
                yaml_key("device"),
                YamlValue::String(ALOOP_CAPTURE_DEVICE.to_owned()),
            );
            capture.insert(yaml_key("stop_on_inactive"), YamlValue::Bool(true));

            let format_was_explicit = existing_capture
                .and_then(|capture| capture.get(yaml_key("format")))
                .map(|value| !value.is_null())
                .unwrap_or(false);
            if format_was_explicit {
                if let Some(format) = wave.sample_format.clone().or(explicit_format) {
                    capture.insert(yaml_key("format"), YamlValue::String(format));
                }
            }
            capture
        }
        RuntimeBackend::Ioplug => {
            let mut capture = base_stdin_capture(existing_capture);
            let format = wave
                .sample_format
                .clone()
                .or(explicit_format)
                .ok_or_else(|| {
                    app_error(
                        "ioplug runtime capture format is missing and no fallback is configured",
                    )
                })?;
            capture.insert(yaml_key("type"), YamlValue::String("Stdin".to_owned()));
            capture.insert(yaml_key("channels"), YamlValue::from(channels as u64));
            capture.insert(yaml_key("format"), YamlValue::String(format));
            capture
        }
    };

    Ok(capture)
}

// ─── Installer utility functions ───────────────────────────────────────────

/// Read `config_path` from a CamillaDSP statefile (YAML).
///
/// CamillaDSP writes a statefile containing the active config path, volume,
/// and mute state.  Parsing it with `serde_yaml_ng` handles all valid YAML
/// representations (quoted, unquoted, flow) and replaces the fragile AWK
/// parser previously used in the shell installer.
pub fn get_config_path(path: &Path) -> AppResult<String> {
    let raw = fs::read_to_string(path).map_err(|err| {
        app_error(format!(
            "unable to read statefile {}: {err}",
            path.display()
        ))
    })?;
    let root: YamlValue = serde_yaml_ng::from_str(&raw)
        .map_err(|err| app_error(format!("invalid YAML in {}: {err}", path.display())))?;
    root.get("config_path")
        .and_then(YamlValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| app_error("statefile has no 'config_path' value"))
}

fn get_state_sequence<'a>(root: &'a YamlValue, key: &str) -> AppResult<&'a [YamlValue]> {
    let seq = root
        .get(key)
        .and_then(YamlValue::as_sequence)
        .ok_or_else(|| app_error(format!("statefile has no '{key}' sequence")))?;
    if seq.len() != CAMILLA_STATE_CHANNELS {
        return Err(app_error(format!(
            "statefile '{key}' must contain exactly {CAMILLA_STATE_CHANNELS} values"
        )));
    }
    Ok(seq)
}

fn validated_mute_block(root: &YamlValue) -> AppResult<YamlValue> {
    let mute = get_state_sequence(root, "mute")?
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            value.as_bool().map(YamlValue::Bool).ok_or_else(|| {
                app_error(format!("statefile 'mute[{}]' must be a boolean", idx + 1))
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(YamlValue::Sequence(mute))
}

fn validated_volume_block(root: &YamlValue) -> AppResult<YamlValue> {
    let volume = get_state_sequence(root, "volume")?
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let number = value
                .as_f64()
                .or_else(|| value.as_i64().map(|v| v as f64))
                .or_else(|| value.as_u64().map(|v| v as f64))
                .filter(|v| v.is_finite())
                .ok_or_else(|| {
                    app_error(format!(
                        "statefile 'volume[{}]' must be a finite number",
                        idx + 1
                    ))
                })?;
            Ok(YamlValue::from(number))
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(YamlValue::Sequence(volume))
}

/// Read a CamillaDSP statefile and return a normalized YAML fragment
/// containing validated `mute` and `volume` blocks.
///
/// This replaces the installer's last AWK-based YAML parsing. Both arrays must
/// exist, contain exactly five entries, and have the expected scalar types.
pub fn get_state_fragment(path: &Path) -> AppResult<String> {
    let raw = fs::read_to_string(path).map_err(|err| {
        app_error(format!(
            "unable to read statefile {}: {err}",
            path.display()
        ))
    })?;
    let root: YamlValue = serde_yaml_ng::from_str(&raw)
        .map_err(|err| app_error(format!("invalid YAML in {}: {err}", path.display())))?;

    let mut fragment = Mapping::new();
    fragment.insert(yaml_key("mute"), validated_mute_block(&root)?);
    fragment.insert(yaml_key("volume"), validated_volume_block(&root)?);
    let yaml = serde_yaml_ng::to_string(&YamlValue::Mapping(fragment))?;
    Ok(yaml.strip_prefix("---\n").unwrap_or(&yaml).to_owned())
}

/// Read the `devices.playback.device` value from a CamillaDSP YAML config.
///
/// Handles both block and flow YAML correctly via `serde_yaml_ng`, replacing
/// the AWK-based `read_cdsp_playback_device` in the installer shell script.
/// Flow YAML like `playback: {type: Alsa, device: "hw:USB,0"}` is parsed
/// correctly, and device names containing special characters are returned
/// verbatim without shell-quoting side-effects.
pub fn get_playback_device(path: &Path) -> AppResult<String> {
    let raw = fs::read_to_string(path)
        .map_err(|err| app_error(format!("unable to read config {}: {err}", path.display())))?;
    let root: YamlValue = serde_yaml_ng::from_str(&raw)
        .map_err(|err| app_error(format!("invalid YAML in {}: {err}", path.display())))?;
    root.get("devices")
        .and_then(|d| d.get("playback"))
        .and_then(|p| p.get("device"))
        .and_then(YamlValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| app_error("config has no 'devices.playback.device' value"))
}

/// Write a bypass CamillaDSP config with the given playback device to a YAML
/// string.
///
/// All string values — including the playback device name — are serialized via
/// `serde_yaml_ng`, which applies correct YAML quoting and escaping regardless
/// of characters like `"`, `'`, or `\` in the device name.  This replaces the
/// shell heredoc that interpolated `${PLAYBACK_DEVICE}` directly, which could
/// produce invalid YAML for unusual device names (issue 20).
///
/// The output is the piCoreDSP *Bypass* configuration, a minimal pass-through
/// config suitable for use as both the initial startup config
/// (`default_config.yml`) and the active config link target (`Bypass.yml`).
pub fn make_bypass_config(playback_device: &str) -> AppResult<String> {
    let mut capture = Mapping::new();
    capture.insert(yaml_key("type"), YamlValue::String("Alsa".to_owned()));
    capture.insert(yaml_key("channels"), YamlValue::from(2u64));
    capture.insert(
        yaml_key("device"),
        YamlValue::String("hw:Loopback,0,0".to_owned()),
    );
    capture.insert(yaml_key("stop_on_inactive"), YamlValue::Bool(true));

    let mut playback = Mapping::new();
    playback.insert(yaml_key("type"), YamlValue::String("Alsa".to_owned()));
    playback.insert(yaml_key("channels"), YamlValue::from(2u64));
    playback.insert(
        yaml_key("device"),
        YamlValue::String(playback_device.to_owned()),
    );

    let mut devices = Mapping::new();
    devices.insert(yaml_key("samplerate"), YamlValue::from(44100u64));
    devices.insert(yaml_key("chunksize"), YamlValue::from(2048u64));
    devices.insert(yaml_key("queuelimit"), YamlValue::from(4u64));
    devices.insert(yaml_key("enable_rate_adjust"), YamlValue::Bool(true));
    devices.insert(yaml_key("capture"), YamlValue::Mapping(capture));
    devices.insert(yaml_key("playback"), YamlValue::Mapping(playback));

    let description = format!(
        "IMPORTANT: This description is longer than the visible CamillaGUI field.\n\
         Scroll down and read it completely before changing or applying this config.\n\n\
         Default piCoreDSP pass-through baseline configuration.\n\n\
         This YAML file is the persistent baseline configuration. It does not\n\
         necessarily contain the parameters currently used by CamillaDSP.\n\n\
         Audio is captured from snd-aloop and played to the piCorePlayer output\n\
         that was selected before installation: {playback_device}\n\n\
         piCoreDSP builds on the upstream camilladsp-controller snd-aloop adaptation\n\
         model, but replaces its Python runtime with the native Rust\n\
         picoredsp-controller and adds piCorePlayer-specific persistence,\n\
         active-config reloading and startup/rate-switch handling.\n\n\
         When playback becomes active, picoredsp-controller reads the live stream\n\
         parameters from snd-aloop and adapts the CamillaDSP configuration in memory\n\
         to the current sample rate and, where applicable, capture format and channel\n\
         count.\n\n\
         Therefore values shown in this YAML or in the CamillaGUI config editor,\n\
         such as samplerate: 44100, may differ from the live values shown in\n\
         CamillaGUI's status area. The live status values are authoritative for the\n\
         stream currently playing.\n\n\
         Runtime-adapted values are not written back to this file.\n\n\
         Save DSP/filter changes in CamillaGUI if they must survive a sample-rate or\n\
         format change or a reboot. Prefer \"Apply and Save\" for persistent changes.\n\n\
         On piCorePlayer, run \"pcp backup\" after persistent configuration changes and\n\
         before rebooting, so the current system configuration is backed up.\n\n\
         Do not manually Apply a configuration merely to initialize DSP while\n\
         playback is stopped. The controller automatically loads and adapts the\n\
         configuration when playback becomes active.\n"
    );

    let mut root = Mapping::new();
    root.insert(yaml_key("devices"), YamlValue::Mapping(devices));
    root.insert(yaml_key("filters"), YamlValue::Mapping(Mapping::new()));
    root.insert(yaml_key("mixers"), YamlValue::Mapping(Mapping::new()));
    root.insert(yaml_key("pipeline"), YamlValue::Sequence(vec![]));
    root.insert(yaml_key("processors"), YamlValue::Mapping(Mapping::new()));
    root.insert(yaml_key("title"), YamlValue::String("Bypass".to_owned()));
    root.insert(yaml_key("description"), YamlValue::String(description));

    Ok(serde_yaml_ng::to_string(&YamlValue::Mapping(root))?)
}

// ─── Config adaptation ─────────────────────────────────────────────────────

/// Adapt a CamillaDSP YAML config for the current wave format and the aloop
/// backend, returning the updated YAML string.
pub fn adapt_config(path: &Path, wave: &WaveFormat) -> AppResult<String> {
    adapt_config_for_backend(path, wave, RuntimeBackend::Aloop)
}

/// Adapt a CamillaDSP YAML config for the current wave format and runtime
/// backend, returning the updated YAML string.
///
/// The path is intentionally **re-read on every call**. Because the installer
/// passes `active_config.yml` (a symlink managed by CamillaGUI), re-reading
/// means a config change made in the GUI is picked up on the very next
/// adaptation without a controller restart. This is the key piCoreDSP
/// extension over upstream `AdaptConfig`, which caches the file at startup.
///
/// Before serializing the adapted config, all relative FIR coefficient
/// filenames are converted to absolute paths (mirroring CamillaGUI's own
/// `make_config_filter_paths_absolute` step).  Without this, SetConfig over
/// WebSocket cannot resolve relative paths because CamillaDSP loses the
/// config-file path context.
///
/// Adaptation rules mirror Python `AdaptConfig._change_*` exactly:
///
/// * **samplerate** — updated in `devices.samplerate` unless a resampler is
///   present, in which case `devices.capture_samplerate` is set instead (and a
///   synchronous 1:1 resampler is removed for this runtime copy).
/// * **capture runtime fields** — `type`, `device`, `format`, `channels`, and
///   `stop_on_inactive` are injected according to the selected backend while
///   preserving user-owned capture fields such as `labels`.
pub fn adapt_config_for_backend(
    path: &Path,
    wave: &WaveFormat,
    backend: RuntimeBackend,
) -> AppResult<String> {
    // Canonicalize to resolve symlinks and get the true config file directory.
    // This is required for correct relative-path resolution in filters.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let config_dir = canonical
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .to_path_buf();

    let raw = fs::read_to_string(path)
        .map_err(|err| app_error(format!("unable to read config {}: {err}", path.display())))?;
    let mut root: YamlValue = serde_yaml_ng::from_str(&raw)?;

    let root_map = mapping_mut(&mut root, "config root")?;
    let devices_value = root_map
        .get_mut(yaml_key("devices"))
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
                .get(yaml_key("type"))
                .and_then(YamlValue::as_str)
                .ok_or_else(|| app_error("devices.resampler.type must be a string"))?
                .to_owned();

            let configured_rate = devices.get(yaml_key("samplerate")).and_then(yaml_u32);

            devices.insert(yaml_key("capture_samplerate"), YamlValue::from(rate as u64));

            if resampler_type == "Synchronous" && configured_rate == Some(rate) {
                devices.insert(resampler_key, YamlValue::Null);
            }
        }
    }

    let existing_capture = existing_capture_mapping(devices)?;
    let runtime_capture = build_runtime_capture(existing_capture, wave, backend)?;
    devices.insert(yaml_key("capture"), YamlValue::Mapping(runtime_capture));

    // Convert relative FIR coefficient filenames to absolute paths, mirroring
    // CamillaGUI's behavior before SetConfig calls.
    make_filter_paths_absolute(&mut root, &config_dir);

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
        let path = env::temp_dir().join(format!("picoredsp-{name}-{}-{stamp}", std::process::id()));
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

    fn portable_base_config(playback: &str) -> String {
        format!(
            "devices:\n  samplerate: 44100\n  chunksize: 2048\n  \
             capture:\n    labels:\n      - Input_L\n      - Input_R\n  \
             playback:\n    type: Alsa\n    channels: 2\n    \
             device: \"{playback}\"\nfilters:\n  Gain:\n    type: Gain\n    parameters:\n      gain: -3.0\nmixers: {{}}\n\
             pipeline:\n  - type: Filter\n    channel: 0\n    names:\n      - Gain\nprocessors: {{}}\n"
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
        assert_eq!(
            parsed["devices"]["capture_samplerate"].as_u64(),
            Some(48000)
        );
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
    fn same_portable_baseline_adapts_for_aloop_and_ioplug() {
        let dir = test_dir("portable-both");
        let config = dir.join("config.yml");
        fs::write(&config, portable_base_config("hw:DAC,0")).unwrap();

        let wave = WaveFormat {
            sample_rate: Some(96_000),
            sample_format: Some("S24_4_LE".to_owned()),
            channels: Some(2),
        };

        let aloop = adapt_config_for_backend(&config, &wave, RuntimeBackend::Aloop).unwrap();
        let ioplug = adapt_config_for_backend(&config, &wave, RuntimeBackend::Ioplug).unwrap();

        let aloop_parsed: YamlValue = serde_yaml_ng::from_str(&aloop).unwrap();
        let ioplug_parsed: YamlValue = serde_yaml_ng::from_str(&ioplug).unwrap();

        assert_eq!(aloop_parsed["devices"]["samplerate"].as_u64(), Some(96_000));
        assert_eq!(
            aloop_parsed["devices"]["capture"]["type"].as_str(),
            Some("Alsa")
        );
        assert_eq!(
            aloop_parsed["devices"]["capture"]["device"].as_str(),
            Some("hw:Loopback,0,0")
        );
        assert_eq!(
            aloop_parsed["devices"]["capture"]["stop_on_inactive"].as_bool(),
            Some(true)
        );
        assert!(aloop_parsed["devices"]["capture"].get("format").is_none());
        assert_eq!(
            aloop_parsed["devices"]["capture"]["labels"][0].as_str(),
            Some("Input_L")
        );

        assert_eq!(
            ioplug_parsed["devices"]["samplerate"].as_u64(),
            Some(96_000)
        );
        assert_eq!(
            ioplug_parsed["devices"]["capture"]["type"].as_str(),
            Some("Stdin")
        );
        assert_eq!(
            ioplug_parsed["devices"]["capture"]["format"].as_str(),
            Some("S24_4_LE")
        );
        assert!(ioplug_parsed["devices"]["capture"].get("device").is_none());
        assert!(ioplug_parsed["devices"]["capture"]
            .get("stop_on_inactive")
            .is_none());
        assert_eq!(
            ioplug_parsed["devices"]["capture"]["labels"][1].as_str(),
            Some("Input_R")
        );

        assert_eq!(
            aloop_parsed["devices"]["playback"]["device"].as_str(),
            Some("hw:DAC,0")
        );
        assert_eq!(
            ioplug_parsed["devices"]["playback"]["device"].as_str(),
            Some("hw:DAC,0")
        );
        assert_eq!(
            aloop_parsed["filters"]["Gain"]["type"].as_str(),
            Some("Gain")
        );
        assert_eq!(
            ioplug_parsed["filters"]["Gain"]["type"].as_str(),
            Some("Gain")
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ioplug_adaptation_removes_stale_aloop_runtime_fields() {
        let dir = test_dir("ioplug-removes-aloop");
        let config = dir.join("config.yml");
        fs::write(&config, base_config("hw:DAC,0", Some("S16_LE"))).unwrap();

        let raw = fs::read_to_string(&config).unwrap();
        fs::write(
            &config,
            raw.replace(
                "    format: S16_LE\n",
                "    format: S16_LE\n    labels:\n      - Input_L\n      - Input_R\n",
            ),
        )
        .unwrap();

        let wave = WaveFormat {
            sample_rate: Some(48_000),
            sample_format: Some("S32_LE".to_owned()),
            channels: Some(2),
        };

        let adapted = adapt_config_for_backend(&config, &wave, RuntimeBackend::Ioplug).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();

        assert_eq!(parsed["devices"]["capture"]["type"].as_str(), Some("Stdin"));
        assert_eq!(
            parsed["devices"]["capture"]["format"].as_str(),
            Some("S32_LE")
        );
        assert!(parsed["devices"]["capture"].get("device").is_none());
        assert!(parsed["devices"]["capture"]
            .get("stop_on_inactive")
            .is_none());
        assert_eq!(
            parsed["devices"]["capture"]["labels"][0].as_str(),
            Some("Input_L")
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ioplug_adaptation_strips_alsa_only_capture_fields_from_camillagui_config() {
        let dir = test_dir("ioplug-strips-alsa-only-fields");
        let config = dir.join("config.yml");
        fs::write(
            &config,
            "devices:\n  samplerate: 44100\n  chunksize: 2048\n  capture:\n    type: Alsa\n    channels: 2\n    device: hw:Loopback,0,0\n    format: S16_LE\n    labels: null\n    link_mute_control: null\n    link_volume_control: null\n    stop_on_inactive: true\n  playback:\n    type: Alsa\n    channels: 2\n    device: plughw:CARD=sndrpihifiberry,DEV=0\nfilters: {}\nmixers: {}\npipeline: []\nprocessors: {}\n",
        )
        .unwrap();

        let wave = WaveFormat {
            sample_rate: Some(44_100),
            sample_format: Some("S16_LE".to_owned()),
            channels: Some(2),
        };

        let adapted = adapt_config_for_backend(&config, &wave, RuntimeBackend::Ioplug).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();
        let capture = parsed["devices"]["capture"].as_mapping().unwrap();

        assert_eq!(
            capture.get(yaml_key("type")).and_then(YamlValue::as_str),
            Some("Stdin")
        );
        assert_eq!(
            capture
                .get(yaml_key("channels"))
                .and_then(YamlValue::as_u64),
            Some(2)
        );
        assert_eq!(
            capture.get(yaml_key("format")).and_then(YamlValue::as_str),
            Some("S16_LE")
        );
        assert!(capture.contains_key(yaml_key("labels")));
        assert!(!capture.contains_key(yaml_key("device")));
        assert!(!capture.contains_key(yaml_key("stop_on_inactive")));
        assert!(!capture.contains_key(yaml_key("link_mute_control")));
        assert!(!capture.contains_key(yaml_key("link_volume_control")));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ioplug_adaptation_requires_format_when_no_runtime_or_baseline_format_exists() {
        let dir = test_dir("ioplug-format-required");
        let config = dir.join("config.yml");
        fs::write(&config, portable_base_config("null")).unwrap();

        let wave = WaveFormat {
            sample_rate: Some(44_100),
            sample_format: None,
            channels: Some(2),
        };

        assert!(adapt_config_for_backend(&config, &wave, RuntimeBackend::Ioplug).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relative_fir_filename_is_made_absolute() {
        // Reproduces the CamillaDSP relative-path bug:
        // configs/MyDSP.yml references ../coeffs/test.wav.
        // When adapt_config() sends the result via SetConfig the path must be
        // absolute so CamillaDSP can find it without config-file path context.
        let dir = test_dir("fir-abs");
        let config_dir = dir.join("configs");
        let coeff_dir = dir.join("coeffs");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&coeff_dir).unwrap();

        let coeff_file = coeff_dir.join("test.wav");
        fs::write(&coeff_file, b"dummy").unwrap();

        let config_content = "devices:\n  samplerate: 44100\n  chunksize: 2048\n  \
             capture:\n    type: Alsa\n    channels: 2\n    \
             device: \"hw:Loopback,0,0\"\n  \
             playback:\n    type: Alsa\n    channels: 2\n    \
             device: \"null\"\n\
             filters:\n  LeftFIR:\n    type: Conv\n    parameters:\n      \
             type: Wav\n      filename: \"../coeffs/test.wav\"\n\
             mixers: {}\npipeline: []\nprocessors: {}\n";
        let config_path = config_dir.join("MyDSP.yml");
        fs::write(&config_path, config_content).unwrap();

        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: None,
            channels: Some(2),
        };
        let adapted = adapt_config(&config_path, &wave).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();

        let filename = parsed["filters"]["LeftFIR"]["parameters"]["filename"]
            .as_str()
            .unwrap();
        assert!(
            std::path::Path::new(filename).is_absolute(),
            "adapted filename should be absolute, got: {filename}"
        );
        // The resolved absolute path should match our coefficient file.
        assert_eq!(filename, coeff_file.to_str().unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn yaml_helpers_accept_normal_camilladsp_config() {
        let value: YamlValue = serde_yaml_ng::from_str(&base_config("null", None)).unwrap();
        let root = mapping(&value, "root").unwrap();
        assert!(root.contains_key(yaml_key("devices")));
    }

    // ── get_playback_device tests ────────────────────────────────────────

    #[test]
    fn get_playback_device_reads_block_yaml() {
        let dir = test_dir("get-dev-block");
        let config = dir.join("config.yml");
        fs::write(&config, base_config("hw:USB,0", None)).unwrap();
        assert_eq!(get_playback_device(&config).unwrap(), "hw:USB,0");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_playback_device_reads_flow_yaml() {
        // Reproduces issue 19: AWK parser fails on flow YAML; serde_yaml_ng handles it.
        let dir = test_dir("get-dev-flow");
        let config = dir.join("config.yml");
        fs::write(
            &config,
            "devices:\n  samplerate: 44100\n  chunksize: 2048\n  \
             capture:\n    type: Alsa\n    channels: 2\n    device: \"hw:Loopback,0,0\"\n  \
             playback: {type: Alsa, channels: 2, device: \"hw:USB,0\"}\n\
             filters: {}\nmixers: {}\npipeline: []\nprocessors: {}\n",
        )
        .unwrap();
        assert_eq!(get_playback_device(&config).unwrap(), "hw:USB,0");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_playback_device_returns_error_when_field_missing() {
        let dir = test_dir("get-dev-missing");
        let config = dir.join("config.yml");
        // Config without a playback device field.
        fs::write(
            &config,
            "devices:\n  samplerate: 44100\n  chunksize: 2048\n  \
             capture:\n    type: Alsa\n    channels: 2\n    device: \"hw:Loopback,0,0\"\n  \
             playback:\n    type: Alsa\n    channels: 2\n\
             filters: {}\nmixers: {}\npipeline: []\nprocessors: {}\n",
        )
        .unwrap();
        assert!(get_playback_device(&config).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    // ── make_bypass_config tests ─────────────────────────────────────────

    #[test]
    fn make_bypass_config_produces_valid_yaml_with_correct_device() {
        let yaml = make_bypass_config("hw:DAC,0").unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(
            parsed["devices"]["playback"]["device"].as_str(),
            Some("hw:DAC,0")
        );
        assert_eq!(parsed["devices"]["samplerate"].as_u64(), Some(44100));
        assert_eq!(
            parsed["devices"]["enable_rate_adjust"].as_bool(),
            Some(true)
        );
        assert_eq!(
            parsed["devices"]["capture"]["device"].as_str(),
            Some("hw:Loopback,0,0")
        );
        assert!(parsed["title"].as_str().is_some());
        assert!(parsed["description"]
            .as_str()
            .unwrap_or_default()
            .contains("persistent baseline configuration"));
        assert!(parsed["description"]
            .as_str()
            .unwrap_or_default()
            .contains("pcp backup"));
    }

    #[test]
    fn make_bypass_config_escapes_special_characters_in_device_name() {
        // Reproduces issue 20: device names with `"` or `\` must not produce
        // invalid YAML when the Rust serializer is used instead of shell heredoc.
        let yaml = make_bypass_config(r#"hw:"Quoted",0"#).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(
            parsed["devices"]["playback"]["device"].as_str(),
            Some(r#"hw:"Quoted",0"#)
        );
    }

    #[test]
    fn make_bypass_config_roundtrips_via_get_playback_device() {
        let dir = test_dir("bypass-roundtrip");
        let config = dir.join("Bypass.yml");
        let device = "hw:CARD=USB_Audio,DEV=0";
        let yaml = make_bypass_config(device).unwrap();
        fs::write(&config, &yaml).unwrap();
        let recovered = get_playback_device(&config).unwrap();
        assert_eq!(recovered, device);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_config_path_reads_unquoted_value() {
        let dir = test_dir("statefile-unquoted");
        let sf = dir.join("state.yml");
        fs::write(
            &sf,
            "config_path: /mnt/camilladsp/MyDSP.yml\nvolume:\n- -10.0\n",
        )
        .unwrap();
        assert_eq!(get_config_path(&sf).unwrap(), "/mnt/camilladsp/MyDSP.yml");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_config_path_reads_quoted_value() {
        let dir = test_dir("statefile-quoted");
        let sf = dir.join("state.yml");
        fs::write(
            &sf,
            "config_path: \"/mnt/camilladsp/My DSP.yml\"\nvolume:\n- -10.0\n",
        )
        .unwrap();
        assert_eq!(get_config_path(&sf).unwrap(), "/mnt/camilladsp/My DSP.yml");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_config_path_errors_when_key_missing() {
        let dir = test_dir("statefile-no-key");
        let sf = dir.join("state.yml");
        fs::write(&sf, "volume:\n- -10.0\n").unwrap();
        assert!(get_config_path(&sf).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_state_fragment_returns_validated_blocks() {
        let dir = test_dir("statefile-fragment");
        let sf = dir.join("state.yml");
        fs::write(
            &sf,
            "config_path: /mnt/camilladsp/Bypass.yml\n\
             mute:\n\
             - false\n\
             - true\n\
             - false\n\
             - true\n\
             - false\n\
             volume:\n\
             - -20.0\n\
             - -10\n\
             - 0.0\n\
             - 1.5\n\
             - 3\n",
        )
        .unwrap();

        let fragment = get_state_fragment(&sf).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&fragment).unwrap();
        let mute = parsed["mute"].as_sequence().unwrap();
        let volume = parsed["volume"].as_sequence().unwrap();

        assert_eq!(mute.len(), CAMILLA_STATE_CHANNELS);
        assert_eq!(volume.len(), CAMILLA_STATE_CHANNELS);
        assert_eq!(mute[1].as_bool(), Some(true));
        assert_eq!(volume[0].as_f64(), Some(-20.0));
        assert_eq!(volume[4].as_f64(), Some(3.0));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_state_fragment_rejects_wrong_array_length() {
        let dir = test_dir("statefile-short-array");
        let sf = dir.join("state.yml");
        fs::write(
            &sf,
            "mute:\n- false\n- false\nvolume:\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n",
        )
        .unwrap();

        let err = get_state_fragment(&sf).unwrap_err().to_string();
        assert!(err.contains("exactly 5 values"));

        fs::remove_dir_all(dir).unwrap();
    }

    // ── make_statefile tests ─────────────────────────────────────────────

    #[test]
    fn make_statefile_first_install_produces_defaults() {
        let yaml = make_statefile("/mnt/camilladsp/Bypass.yml", None).unwrap();
        let sf: StateFile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(
            sf.config_path.as_deref(),
            Some("/mnt/camilladsp/Bypass.yml")
        );
        assert_eq!(sf.mute, [false; 5]);
        assert_eq!(sf.volume, [0.0_f32; 5]);
    }

    #[test]
    fn make_statefile_reinstall_preserves_mute_and_volume() {
        let dir = test_dir("make-state-reinstall");
        let old_sf = dir.join("old_state.yml");
        let original_mute = [true, false, true, false, true];
        let original_volume = [-10.0_f32, -5.0, 0.0, 1.5, -20.5];
        let original_path = "/mnt/camilladsp/My DSP.yml";
        let existing = StateFile {
            config_path: Some(original_path.to_owned()),
            mute: original_mute,
            volume: original_volume,
        };
        fs::write(&old_sf, serde_yaml_ng::to_string(&existing).unwrap()).unwrap();

        let new_config_path = "/mnt/camilladsp/New DSP.yml";
        let yaml = make_statefile(new_config_path, Some(&old_sf)).unwrap();
        let loaded: StateFile = serde_yaml_ng::from_str(&yaml).unwrap();

        assert_eq!(loaded.config_path.as_deref(), Some(new_config_path));
        assert_eq!(loaded.mute, original_mute);
        assert_eq!(loaded.volume, original_volume);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_statefile_reinstall_roundtrip_same_path() {
        let dir = test_dir("make-state-roundtrip");
        let old_sf = dir.join("state.yml");
        let original_path = "/mnt/camilladsp/Bypass.yml";
        let original_mute = [false, true, false, false, true];
        let original_volume = [0.0_f32, -3.0, -6.0, -9.0, -12.0];
        let existing = StateFile {
            config_path: Some(original_path.to_owned()),
            mute: original_mute,
            volume: original_volume,
        };
        fs::write(&old_sf, serde_yaml_ng::to_string(&existing).unwrap()).unwrap();

        let yaml = make_statefile(original_path, Some(&old_sf)).unwrap();
        let loaded: StateFile = serde_yaml_ng::from_str(&yaml).unwrap();

        assert_eq!(loaded.config_path.as_deref(), Some(original_path));
        assert_eq!(loaded.mute, original_mute);
        assert_eq!(loaded.volume, original_volume);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_statefile_existing_state_null_config_path_is_accepted() {
        // CamillaDSP writes config_path: null after --no_config boot.
        // make_statefile must parse such a statefile without error.
        let dir = test_dir("make-state-null-config");
        let sf = dir.join("state.yml");
        fs::write(
            &sf,
            "config_path: null\n\
             mute:\n- false\n- false\n- false\n- false\n- false\n\
             volume:\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n",
        )
        .unwrap();
        let new_path = "/mnt/camilladsp/Bypass.yml";
        let yaml = make_statefile(new_path, Some(&sf)).unwrap();
        let loaded: StateFile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(loaded.config_path.as_deref(), Some(new_path));
        assert_eq!(loaded.mute, [false; 5]);
        assert_eq!(loaded.volume, [0.0_f32; 5]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_statefile_existing_state_missing_file_is_error() {
        let result = make_statefile(
            "/mnt/camilladsp/Bypass.yml",
            Some(Path::new("/nonexistent/state.yml")),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unable to read"));
    }

    #[test]
    fn make_statefile_existing_state_wrong_mute_length_is_error() {
        let dir = test_dir("make-state-bad-len");
        let sf = dir.join("state.yml");
        // Only 3 mute values instead of 5 — serde must reject this.
        fs::write(
            &sf,
            "config_path: /mnt/camilladsp/Bypass.yml\n\
             mute:\n- false\n- false\n- false\n\
             volume:\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n",
        )
        .unwrap();
        let result = make_statefile("/mnt/camilladsp/Bypass.yml", Some(&sf));
        assert!(result.is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_statefile_existing_state_wrong_volume_type_is_error() {
        let dir = test_dir("make-state-bad-type");
        let sf = dir.join("state.yml");
        // One volume entry is a string — serde must reject this.
        fs::write(
            &sf,
            "config_path: /mnt/camilladsp/Bypass.yml\n\
             mute:\n- false\n- false\n- false\n- false\n- false\n\
             volume:\n- 0.0\n- notanumber\n- 0.0\n- 0.0\n- 0.0\n",
        )
        .unwrap();
        let result = make_statefile("/mnt/camilladsp/Bypass.yml", Some(&sf));
        assert!(result.is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_statefile_existing_state_invalid_yaml_is_error() {
        let dir = test_dir("make-state-bad-yaml");
        let sf = dir.join("state.yml");
        fs::write(&sf, "{ not valid yaml: [").unwrap();
        let result = make_statefile("/mnt/camilladsp/Bypass.yml", Some(&sf));
        assert!(result.is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_statefile_existing_state_nan_volume_is_error() {
        let dir = test_dir("make-state-nan-volume");
        let sf = dir.join("state.yml");
        fs::write(
            &sf,
            "config_path: /mnt/camilladsp/Bypass.yml\n\
             mute:\n- false\n- false\n- false\n- false\n- false\n\
             volume:\n- 0.0\n- .nan\n- 0.0\n- 0.0\n- 0.0\n",
        )
        .unwrap();
        let err = make_statefile("/mnt/camilladsp/Bypass.yml", Some(&sf))
            .unwrap_err()
            .to_string();
        assert!(err.contains("volume values must be finite"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_statefile_existing_state_infinite_volume_is_error() {
        let dir = test_dir("make-state-inf-volume");
        let sf = dir.join("state.yml");
        fs::write(
            &sf,
            "config_path: /mnt/camilladsp/Bypass.yml\n\
             mute:\n- false\n- false\n- false\n- false\n- false\n\
             volume:\n- 0.0\n- .inf\n- 0.0\n- 0.0\n- 0.0\n",
        )
        .unwrap();
        let err = make_statefile("/mnt/camilladsp/Bypass.yml", Some(&sf))
            .unwrap_err()
            .to_string();
        assert!(err.contains("volume values must be finite"));
        fs::remove_dir_all(dir).unwrap();
    }

    /// Verify that config_path values containing YAML-significant characters
    /// are correctly round-tripped through the statefile serializer.
    /// These are exactly the filenames that caused problems with shell heredoc
    /// interpolation or YAML plain-scalar parsing.
    #[test]
    fn make_statefile_special_config_path_filenames() {
        let tricky_names = [
            "My DSP #1.yml",
            "My: DSP.yml",
            "\"quoted\".yml",
            "[room].yml",
            "{room}.yml",
            "-room.yml",
            "room's dsp.yml",
        ];
        for name in &tricky_names {
            let path = format!("/mnt/camilladsp/{name}");
            let yaml = make_statefile(&path, None).unwrap();
            let loaded: StateFile = serde_yaml_ng::from_str(&yaml).unwrap();
            assert_eq!(
                loaded.config_path.as_deref(),
                Some(path.as_str()),
                "round-trip failed for config_path: {path}"
            );
        }
    }

    /// Regression test for the symlink baseline preservation invariant.
    ///
    /// Finding #1 identified that if `--adapt` points to a symlink
    /// (`active_config.yml → MyDSP.yml`) and the controller were to call
    /// `fs::write(&adapt_path, …)`, the write would follow the symlink and
    /// permanently corrupt the user's configuration.
    ///
    /// This test verifies that `adapt_config_for_backend` — the function used
    /// in the ioplug controller — is a **pure** reader: it resolves the symlink
    /// to read the baseline but never writes back to either the symlink or its
    /// target, across multiple successive adaptations.
    #[test]
    fn adapt_config_for_backend_via_symlink_never_writes_baseline() {
        let dir = test_dir("symlink-baseline");
        let baseline = dir.join("MyDSP.yml");
        let active = dir.join("active_config.yml");

        // Write a minimal config with a fixed sample rate.
        let original_content = base_config("hw:DAC,0", Some("S32_LE"));
        fs::write(&baseline, &original_content).unwrap();

        // Simulate the installer: active_config.yml is a symlink → MyDSP.yml.
        symlink(&baseline, &active).unwrap();

        // Simulate three successive streams at different sample rates.
        for rate in [44_100u32, 96_000, 48_000] {
            let wave = WaveFormat {
                sample_rate: Some(rate),
                sample_format: Some("S32_LE".to_owned()),
                channels: Some(2),
            };
            // The controller reads through the symlink to build an adapted string.
            let adapted = adapt_config_for_backend(&active, &wave, RuntimeBackend::Ioplug)
                .unwrap_or_else(|e| panic!("adapt_config_for_backend failed at {rate} Hz: {e}"));

            // The adapted string must contain the stream's rate (sanity check).
            let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();
            let adapted_rate = parsed["devices"]["capture_samplerate"]
                .as_u64()
                .or_else(|| parsed["devices"]["samplerate"].as_u64())
                .expect("adapted YAML has no samplerate");
            assert_eq!(
                adapted_rate, rate as u64,
                "adapted rate mismatch at {rate} Hz"
            );
        }

        // After all three adaptations the baseline file must be byte-for-byte unchanged.
        let after = fs::read_to_string(&baseline)
            .expect("baseline file should still exist after adaptations");
        assert_eq!(
            after, original_content,
            "adapt_config_for_backend must not modify the baseline file"
        );

        // The symlink must still point to the original target (not been replaced).
        assert_eq!(
            active.canonicalize().unwrap(),
            baseline.canonicalize().unwrap(),
            "active_config.yml symlink must still resolve to the original baseline"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Statefile with an unexpected extra key must be rejected even though all
    /// known fields are present and valid.  This mirrors CamillaDSP's own
    /// `#[serde(deny_unknown_fields)]` behaviour so that our installer treats
    /// an unknown-field statefile as invalid rather than silently ignoring it.
    #[test]
    fn make_statefile_rejects_unknown_existing_state_fields() {
        let dir = test_dir("make-state-unknown-field");
        let sf = dir.join("state.yml");
        fs::write(
            &sf,
            "config_path: null\n\
             mute:\n- false\n- false\n- false\n- false\n- false\n\
             volume:\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n\
             unexpected: true\n",
        )
        .unwrap();
        let result = make_statefile("/mnt/camilladsp/Bypass.yml", Some(&sf));
        assert!(
            result.is_err(),
            "statefile with unknown field must be rejected"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
