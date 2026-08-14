use crate::args::Args;
use crate::backend::aloop::AloopBackend;
use crate::backend::ioplug::IoplugBackend;
use crate::backend::ControllerBackend;
use crate::camilladsp::alsa_capture::AlsaLoopbackListener;
use crate::camilladsp::supervisor::StdinSupervisor;
use crate::camilladsp::websocket::CamillaWs;
use crate::core::adaptation::adapt_config_for_backend;
use crate::core::adaptation::RuntimeBackend;
use crate::core::config::{DeviceSnapshot, WaveFormat};
use crate::core::errors::{app_error, AppResult};
use crate::core::logging::{log, LogLevel};
use crate::core::recovery::{ConfigFingerprint, RetryState};
pub use crate::core::state_machine::Controller;
use crate::ipc::protocol::ErrorCode;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type AloopController = Controller<AloopBackend<AlsaLoopbackListener>, CamillaWs>;

/// Production wiring for the current aloop backend profile.
pub fn new_aloop_controller(args: &Args) -> AppResult<(AloopController, DeviceSnapshot)> {
    let listener = AlsaLoopbackListener::new(&args.device, args.log_level)?;
    let initial = listener.read_snapshot()?;
    let client = CamillaWs::connect(&args.host, args.port)?;

    let fallback_wave = WaveFormat {
        sample_rate: args.initial_rate,
        sample_format: args.initial_format.clone(),
        channels: args.initial_channels,
    };
    let adapt_path = args
        .adapt
        .clone()
        .ok_or_else(|| app_error("--adapt is required in controller mode"))?;
    let current_wave = initial.wave.with_fallback(&fallback_wave);
    let stream_backend = AloopBackend::new(listener, initial.clone(), fallback_wave.clone());

    Ok((
        AloopController::new(
            client,
            stream_backend,
            adapt_path,
            fallback_wave,
            current_wave,
            args.log_level,
        ),
        initial,
    ))
}

/// How long after spawning CamillaDSP to verify it is still running.
const STARTUP_CHECK_TIMEOUT: Duration = Duration::from_millis(500);
/// Poll interval used during the startup health-check.
const STARTUP_CHECK_POLL: Duration = Duration::from_millis(50);
/// Transient per-stream runtime YAML written for the ioplug backend.
const IOPLUG_RUNTIME_CONFIG_NAME: &str = "camilladsp_runtime.yml";

fn ioplug_runtime_config_path(socket_path: &Path) -> PathBuf {
    socket_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(IOPLUG_RUNTIME_CONFIG_NAME)
}

fn write_runtime_config(path: &Path, yaml: &str) -> AppResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    fs::create_dir_all(parent).map_err(|err| {
        app_error(format!(
            "unable to create runtime config directory '{}': {err}",
            parent.display()
        ))
    })?;

    let tmp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("runtime"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    fs::write(&tmp_path, yaml).map_err(|err| {
        app_error(format!(
            "unable to write runtime config '{}': {err}",
            tmp_path.display()
        ))
    })?;

    fs::rename(&tmp_path, path).map_err(|err| {
        let _ = fs::remove_file(&tmp_path);
        app_error(format!(
            "unable to install runtime config '{}': {err}",
            path.display()
        ))
    })?;

    Ok(())
}

/// Run the ioplug controller loop (Gate 8 + M9 recovery).
///
/// For each stream:
/// 1. Respect any active retry backoff (exponential, 500 ms–30 s).
/// 2. Wait for the plugin to connect and send START.
/// 3. Adapt the CamillaDSP config for the negotiated stream parameters.
///    — Validation failure latches the retry state until the config changes.
/// 4. Spawn CamillaDSP with the pipe read-end as stdin.
/// 5. Startup timeout check: verify the process is still alive after a short
///    delay; an immediate exit is treated as a transient failure.
/// 6. Send READY to the plugin, delivering the pipe write-end via SCM_RIGHTS.
/// 7. Monitor the stream; wait for STOP or plugin disconnect.
///    — Unexpected CamillaDSP exit is recorded as a transient failure.
/// 8. Close our copy of the pipe write-end → CamillaDSP sees EOF → exits.
///    — Clean STOP resets the retry counter.
pub fn run_ioplug(args: &Args) -> AppResult<()> {
    let socket_path = args
        .socket_path
        .clone()
        .ok_or_else(|| app_error("--socket-path is required for --backend ioplug"))?;
    let adapt_path = args
        .adapt
        .clone()
        .ok_or_else(|| app_error("--adapt is required in controller mode"))?;
    let camilladsp_binary = args
        .camilladsp_binary
        .clone()
        .ok_or_else(|| app_error("--camilladsp is required for --backend ioplug"))?;
    let log_level = args.log_level;
    let runtime_config_path = ioplug_runtime_config_path(&socket_path);

    let mut backend = IoplugBackend::new(&socket_path, log_level)?;
    let mut supervisor = {
        let mut cdsp_extra_args = vec![
            "--port".to_owned(),
            args.port.to_string(),
            "--address".to_owned(),
            args.host.clone(),
        ];
        if let Some(sf) = &args.cdsp_statefile {
            cdsp_extra_args.push("--statefile".to_owned());
            cdsp_extra_args.push(sf.to_string_lossy().into_owned());
        }
        StdinSupervisor::new(&camilladsp_binary, &runtime_config_path, log_level)
            .with_cdsp_args(cdsp_extra_args)
    };

    let mut retry = RetryState::new();
    let mut last_fingerprint = ConfigFingerprint::sample(&adapt_path);

    log(
        LogLevel::Info,
        log_level,
        format!(
            "ioplug controller started; socket={} baseline={} runtime={} camilladsp={}",
            socket_path.display(),
            adapt_path.display(),
            runtime_config_path.display(),
            camilladsp_binary.display(),
        ),
    );

    loop {
        // ── Respect backoff / check for config changes ─────────────────
        //
        // If a retry latch or backoff window is active we spin here,
        // polling the config fingerprint to detect a file change that would
        // clear a permanent latch.  We also accept new plugin connections
        // during the wait so that the plugin receives an immediate error
        // rather than timing out.
        while !retry.should_attempt() {
            let new_fp = ConfigFingerprint::sample(&adapt_path);
            if new_fp != last_fingerprint {
                last_fingerprint = new_fp;
                log(
                    LogLevel::Info,
                    log_level,
                    "ioplug: config file changed — clearing retry latch",
                );
                retry.reset();
                break;
            }
            // Service incoming connections during backoff: accept and
            // immediately reject so the plugin gets an error response rather
            // than sitting blocked until its own connection timeout fires.
            match backend.poll_event(100) {
                Ok(Some(crate::backend::StreamEvent::Started(_))) => {
                    log(
                        LogLevel::Warning,
                        log_level,
                        "ioplug: plugin connected during backoff — rejecting with error",
                    );
                    backend.send_error_to_plugin(ErrorCode::Internal);
                }
                Ok(_) | Err(_) => {
                    // No new connection yet (or a transient IPC error); the
                    // poll_event already slept for the requested timeout so
                    // no extra sleep is needed here.
                }
            }
        }

        // ── Wait for a plugin to connect and send START ────────────────
        let wave = loop {
            use crate::backend::ControllerBackend;
            match backend.poll_event(200)? {
                Some(crate::backend::StreamEvent::Started(params)) => {
                    break WaveFormat {
                        sample_rate: Some(params.rate),
                        sample_format: Some(params.format),
                        channels: Some(params.channels),
                    };
                }
                _ => {
                    // While waiting for a new START, check if a config change
                    // cleared a latch so we can re-enter the backoff check.
                    if retry.latch_until_change {
                        let new_fp = ConfigFingerprint::sample(&adapt_path);
                        if new_fp != last_fingerprint {
                            last_fingerprint = new_fp;
                            log(
                                LogLevel::Info,
                                log_level,
                                "ioplug: config file changed — clearing retry latch",
                            );
                            retry.reset();
                        }
                    }
                    continue;
                }
            }
        };

        log(
            LogLevel::Info,
            log_level,
            format!(
                "ioplug: START received — rate={} format={} channels={}",
                wave.sample_rate.unwrap_or(0),
                wave.sample_format.as_deref().unwrap_or("?"),
                wave.channels.unwrap_or(0),
            ),
        );

        // ── Adapt the baseline config and write a transient runtime copy ──
        // adapt_config_for_backend is a pure function — it returns the
        // adapted YAML string without modifying the file. For the ioplug
        // backend CamillaDSP is spawned from a file on disk, so the adapted
        // YAML must be written to a transient runtime path before the spawn.
        match adapt_config_for_backend(&adapt_path, &wave, RuntimeBackend::Ioplug) {
            Ok(adapted) => {
                if let Err(err) = write_runtime_config(&runtime_config_path, &adapted) {
                    log(
                        LogLevel::Error,
                        log_level,
                        format!(
                            "ioplug: failed to write runtime config to '{}': {err}",
                            runtime_config_path.display()
                        ),
                    );
                    retry.latch();
                    backend.send_error_to_plugin(ErrorCode::Config);
                    continue;
                }
            }
            Err(err) => {
                log(
                    LogLevel::Error,
                    log_level,
                    format!("ioplug: config adaptation failed: {err}"),
                );
                // Permanent latch — do not retry until the config changes.
                retry.latch();
                backend.send_error_to_plugin(ErrorCode::Config);
                continue;
            }
        }

        // ── Spawn CamillaDSP with the pipe as stdin ────────────────────
        let pipe_write_fd = match supervisor.start_stream() {
            Ok(fd) => fd,
            Err(err) => {
                log(
                    LogLevel::Error,
                    log_level,
                    format!("ioplug: failed to spawn CamillaDSP: {err}"),
                );
                retry.record_attempt();
                backend.send_error_to_plugin(ErrorCode::Internal);
                continue;
            }
        };

        // ── Startup timeout check ──────────────────────────────────────
        //
        // Wait a short window to detect an immediate crash (bad config,
        // device unavailable, wrong binary, etc.).  A genuine startup takes
        // milliseconds; if the process exits within the window it failed.
        //
        // An immediate CamillaDSP exit almost always indicates a config or
        // device problem (bad DSP graph, DAC unavailable).  Treat it as a
        // permanent failure and latch until the config changes, rather than
        // as a transient failure that retries with exponential backoff.
        if !supervisor.startup_check(STARTUP_CHECK_TIMEOUT, STARTUP_CHECK_POLL) {
            log(
                LogLevel::Error,
                log_level,
                "ioplug: CamillaDSP exited immediately after spawn — \
                 treating as config/device error (latching until config change)",
            );
            // Latch (not transient retry): CamillaDSP rejected the config or
            // could not open the playback device.  Retrying with the same
            // config will always fail.
            retry.latch();
            supervisor.stop_stream();
            backend.send_error_to_plugin(ErrorCode::Config);
            continue;
        }

        log(
            LogLevel::Debug,
            log_level,
            "ioplug: CamillaDSP startup check passed",
        );

        // ── Send READY + pipe write-end to the plugin ─────────────────
        if let Err(err) = backend.send_ready_with_fd_to_plugin(pipe_write_fd) {
            log(
                LogLevel::Error,
                log_level,
                format!("ioplug: failed to send READY+fd: {err}"),
            );
            retry.record_attempt();
            supervisor.stop_stream();
            continue;
        }

        log(
            LogLevel::Info,
            log_level,
            "ioplug: stream active — waiting for STOP",
        );

        // ── Monitor stream health and wait for STOP ────────────────────
        let mut clean_stop = false;
        loop {
            use crate::backend::ControllerBackend;

            // Check if CamillaDSP died unexpectedly.
            if !supervisor.is_running() {
                log(
                    LogLevel::Error,
                    log_level,
                    "ioplug: CamillaDSP exited unexpectedly during stream",
                );
                // The plugin will get a write error on its pipe fd and
                // should report EPIPE to the ALSA layer.
                break;
            }

            match backend.poll_event(200)? {
                Some(crate::backend::StreamEvent::Stopped) => {
                    clean_stop = true;
                    break;
                }
                _ => continue,
            }
        }

        log(
            LogLevel::Info,
            log_level,
            format!("ioplug: stream ended (clean={})", clean_stop),
        );

        // ── Shut down CamillaDSP ───────────────────────────────────────
        // Closing our write-end sends EOF once the plugin also closes its copy.
        supervisor.stop_stream();

        if clean_stop {
            // Normal end-of-stream: clear backoff counters for the next stream.
            retry.reset();
        } else {
            // CamillaDSP exited mid-stream: record a transient failure and
            // apply backoff before accepting the next connection.
            retry.record_attempt();
            log(
                LogLevel::Warning,
                log_level,
                format!(
                    "ioplug: transient failure #{} — next attempt in ~{}s",
                    retry.consecutive,
                    retry.consecutive.min(6) * 5,
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "picoredsp-controller-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn ioplug_runtime_config_path_uses_socket_directory() {
        let runtime = ioplug_runtime_config_path(Path::new("/run/picoredsp/control.sock"));
        assert_eq!(runtime, Path::new("/run/picoredsp/camilladsp_runtime.yml"));
    }

    #[test]
    fn ioplug_runtime_config_path_falls_back_to_current_directory_for_bare_socket_name() {
        let runtime = ioplug_runtime_config_path(Path::new("control.sock"));
        assert_eq!(runtime, Path::new("./camilladsp_runtime.yml"));
    }

    #[test]
    fn writing_runtime_config_does_not_overwrite_active_config_symlink_target() {
        let dir = test_dir("runtime-config");
        let baseline = dir.join("MyDSP.yml");
        let active = dir.join("active_config.yml");
        let runtime = dir.join("camilladsp_runtime.yml");

        fs::write(&baseline, "devices:\n  samplerate: 44100\n").unwrap();
        symlink(&baseline, &active).unwrap();

        write_runtime_config(&runtime, "devices:\n  samplerate: 96000\n").unwrap();

        assert_eq!(
            fs::read_to_string(&baseline).unwrap(),
            "devices:\n  samplerate: 44100\n"
        );
        assert_eq!(
            active.canonicalize().unwrap(),
            baseline.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read_to_string(&runtime).unwrap(),
            "devices:\n  samplerate: 96000\n"
        );

        fs::remove_dir_all(dir).unwrap();
    }
}
