use crate::args::Args;
use crate::backend::aloop::AloopBackend;
use crate::backend::ioplug::IoplugBackend;
use crate::backend::ControllerBackend;
use crate::camilladsp::alsa_capture::AlsaLoopbackListener;
use crate::camilladsp::supervisor::StdinSupervisor;
use crate::camilladsp::websocket::CamillaWs;
use crate::core::adaptation::adapt_config_for_backend;
use crate::core::adaptation::make_statefile;
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

/// Prime the CamillaDSP statefile with the adapted runtime config path.
///
/// CamillaDSP v4 uses `config_path` from the statefile **instead of** the
/// positional command-line argument when `--statefile` is provided.  If
/// CamillaGUI changes the active config during an active stream (via
/// WebSocket), CamillaDSP rewrites the statefile with the new config path.
/// Without this priming step, the subsequent spawn would load the
/// GUI-selected (unadapted) config instead of the per-stream adapted runtime
/// config, causing an immediate startup failure.
///
/// Existing `mute` and `volume` values are preserved when the statefile can
/// be read and parsed; otherwise safe defaults (`false` / `0.0`) are used.
/// This is a best-effort operation: any failure is logged as a warning and
/// does not abort the stream attempt.
fn prime_cdsp_statefile(statefile_path: &Path, runtime_config_path: &Path, log_level: LogLevel) {
    let canon = match fs::canonicalize(runtime_config_path) {
        Ok(p) => p,
        Err(err) => {
            log(
                LogLevel::Warning,
                log_level,
                format!(
                    "ioplug: cannot canonicalize runtime config '{}': {err} \
                     — skipping statefile prime",
                    runtime_config_path.display()
                ),
            );
            return;
        }
    };
    let config_path_str = canon.to_string_lossy().into_owned();

    // Try to preserve existing mute/volume; fall back to safe defaults if the
    // statefile is missing, unreadable, or has an unexpected schema.
    let yaml = match make_statefile(&config_path_str, Some(statefile_path)) {
        Ok(y) => y,
        Err(err) => {
            log(
                LogLevel::Warning,
                log_level,
                format!(
                    "ioplug: could not read existing statefile '{}' ({err}); \
                     resetting mute/volume to defaults",
                    statefile_path.display()
                ),
            );
            match make_statefile(&config_path_str, None) {
                Ok(y) => y,
                Err(err2) => {
                    log(
                        LogLevel::Warning,
                        log_level,
                        format!(
                            "ioplug: statefile prime failed: {err2} — proceeding without update"
                        ),
                    );
                    return;
                }
            }
        }
    };

    if let Err(err) = write_runtime_config(statefile_path, &yaml) {
        log(
            LogLevel::Warning,
            log_level,
            format!(
                "ioplug: failed to write primed statefile '{}': {err} \
                 — proceeding without update",
                statefile_path.display()
            ),
        );
    } else {
        log(
            LogLevel::Debug,
            log_level,
            format!(
                "ioplug: statefile '{}' primed with runtime config '{}'",
                statefile_path.display(),
                canon.display()
            ),
        );
    }
}

/// Run the ioplug controller loop (Gate 8 + M9 recovery).
///
/// For each stream:
/// 1. Respect any active retry backoff (exponential, 500 ms–30 s).
/// 2. Wait for the plugin to connect and send START.
/// 3. Adapt the CamillaDSP config for the negotiated stream parameters.
///    — Validation failure latches the retry state until the config changes.
/// 4. Prime the statefile (if configured) so CamillaDSP always starts with
///    the adapted runtime config, not a stale GUI-selected config.
/// 5. Spawn CamillaDSP with the pipe read-end as stdin.
/// 6. Startup timeout check: verify the process is still alive after a short
///    delay; an immediate exit is recorded as a transient failure (with
///    exponential backoff) rather than a permanent latch.
/// 7. Send READY to the plugin, delivering the pipe write-end via SCM_RIGHTS.
/// 8. Monitor the stream; wait for STOP or plugin disconnect.
///    — Unexpected CamillaDSP exit is recorded as a transient failure.
/// 9. Close our copy of the pipe write-end → CamillaDSP sees EOF → exits.
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
            "--logfile".to_owned(),
            "/tmp/camilladsp.log".to_owned(),
            "--log_rotate_size".to_owned(),
            "262144".to_owned(),
            "--log_keep_nbr".to_owned(),
            "1".to_owned(),
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

        // ── Prime the CamillaDSP statefile ────────────────────────────
        // CamillaDSP v4 reads config_path from the statefile instead of the
        // positional argument.  CamillaGUI rewrites the statefile whenever the
        // user selects a new config via WebSocket, so without priming, the
        // next spawn would load the GUI-selected (unadapted) config rather
        // than the adapted runtime config, causing an immediate startup
        // failure and an unrecoverable permanent latch.
        if let Some(sf) = &args.cdsp_statefile {
            prime_cdsp_statefile(sf, &runtime_config_path, log_level);
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
        // Treat as a transient failure with exponential backoff rather than a
        // permanent latch: the exit could be caused by device contention, a
        // timing race on slow hardware (SD-card piCorePlayer), or a stale
        // statefile that slipped past the priming step.  Backoff caps at 30 s,
        // and the config-fingerprint change detection resets the counter when
        // the user selects a new config in CamillaGUI.
        if !supervisor.startup_check(STARTUP_CHECK_TIMEOUT, STARTUP_CHECK_POLL) {
            log(
                LogLevel::Error,
                log_level,
                "ioplug: CamillaDSP exited immediately after spawn — \
                 recording transient failure (exponential backoff)",
            );
            retry.record_attempt();
            supervisor.stop_stream();
            backend.send_error_to_plugin(ErrorCode::Internal);
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

        // SCM_RIGHTS has duplicated the write-end into the plugin.  Do not keep
        // a second writer in the controller: otherwise a lost/closed plugin fd
        // cannot produce EOF at CamillaDSP stdin and the process remains
        // misleadingly alive in Stalled state.
        supervisor.release_controller_write_end();

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
            let backoff = retry
                .scheduled_delay()
                .unwrap_or_else(|| std::time::Duration::from_secs(0));
            log(
                LogLevel::Warning,
                log_level,
                format!(
                    "ioplug: transient failure #{} — next attempt in ~{}ms",
                    retry.consecutive,
                    backoff.as_millis(),
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

    // ── prime_cdsp_statefile tests ───────────────────────────────────────

    #[test]
    fn prime_cdsp_statefile_creates_statefile_when_absent() {
        let dir = test_dir("prime-absent");
        let runtime = dir.join("camilladsp_runtime.yml");
        let statefile = dir.join("camilladsp_statefile.yml");

        // Write a dummy runtime config so canonicalize succeeds.
        fs::write(&runtime, "dummy").unwrap();
        assert!(!statefile.exists());

        prime_cdsp_statefile(&statefile, &runtime, LogLevel::Error);

        assert!(
            statefile.exists(),
            "statefile must be created when it did not exist"
        );
        let content = fs::read_to_string(&statefile).unwrap();
        let canon = runtime.canonicalize().unwrap();
        assert!(
            content.contains(canon.to_string_lossy().as_ref()),
            "statefile must reference the runtime config path; got:\n{content}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prime_cdsp_statefile_updates_config_path_in_existing_statefile() {
        let dir = test_dir("prime-update");
        let runtime = dir.join("camilladsp_runtime.yml");
        let statefile = dir.join("camilladsp_statefile.yml");

        fs::write(&runtime, "dummy").unwrap();

        // Write an existing statefile that points to a stale GUI-selected config.
        let stale_yaml = "config_path: /mnt/camilladsp/configs/ConfigB.yml\nmute:\n- true\n- false\n- false\n- false\n- false\nvolume:\n- -3.0\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n";
        fs::write(&statefile, stale_yaml).unwrap();

        prime_cdsp_statefile(&statefile, &runtime, LogLevel::Error);

        let content = fs::read_to_string(&statefile).unwrap();
        let canon = runtime.canonicalize().unwrap();
        assert!(
            content.contains(canon.to_string_lossy().as_ref()),
            "config_path must be updated to runtime config; got:\n{content}"
        );
        // mute/volume must be preserved.
        assert!(
            content.contains("- true"),
            "mute[0]=true must be preserved; got:\n{content}"
        );
        assert!(
            content.contains("-3.0"),
            "volume[0]=-3.0 must be preserved; got:\n{content}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prime_cdsp_statefile_falls_back_to_defaults_when_statefile_malformed() {
        let dir = test_dir("prime-malformed");
        let runtime = dir.join("camilladsp_runtime.yml");
        let statefile = dir.join("camilladsp_statefile.yml");

        fs::write(&runtime, "dummy").unwrap();
        // Write a malformed statefile (unknown fields — serde deny_unknown_fields).
        fs::write(
            &statefile,
            "config_path: /mnt/camilladsp/configs/Foo.yml\nextra_field: bad\n",
        )
        .unwrap();

        prime_cdsp_statefile(&statefile, &runtime, LogLevel::Error);

        // Must not panic; statefile must be updated with runtime config.
        let content = fs::read_to_string(&statefile).unwrap();
        let canon = runtime.canonicalize().unwrap();
        assert!(
            content.contains(canon.to_string_lossy().as_ref()),
            "config_path must be updated even when existing statefile is malformed; got:\n{content}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prime_cdsp_statefile_is_noop_when_runtime_config_missing() {
        let dir = test_dir("prime-missing-runtime");
        let runtime = dir.join("does_not_exist.yml");
        let statefile = dir.join("camilladsp_statefile.yml");

        // statefile should not be created when runtime config doesn't exist.
        prime_cdsp_statefile(&statefile, &runtime, LogLevel::Error);

        assert!(
            !statefile.exists(),
            "statefile must not be created when runtime config is missing"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prime_cdsp_statefile_null_config_path_in_existing_statefile_is_accepted() {
        // CamillaDSP writes `config_path: null` when started with --no_config.
        let dir = test_dir("prime-null-config");
        let runtime = dir.join("camilladsp_runtime.yml");
        let statefile = dir.join("camilladsp_statefile.yml");

        fs::write(&runtime, "dummy").unwrap();
        let null_yaml =
            "config_path: null\nmute:\n- false\n- false\n- false\n- false\n- false\nvolume:\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n- 0.0\n";
        fs::write(&statefile, null_yaml).unwrap();

        prime_cdsp_statefile(&statefile, &runtime, LogLevel::Error);

        let content = fs::read_to_string(&statefile).unwrap();
        let canon = runtime.canonicalize().unwrap();
        assert!(
            content.contains(canon.to_string_lossy().as_ref()),
            "config_path must be updated from null to runtime config; got:\n{content}"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
