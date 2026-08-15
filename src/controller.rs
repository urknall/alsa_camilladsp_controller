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

/// Latch the retry state after a permanent (config/device) failure, and
/// re-sample the config fingerprint so the *failing* config becomes the new
/// baseline for change detection.
///
/// Without this, `last_fingerprint` would remain whatever it was the last
/// time a change was detected (which may be long before the failure — e.g.
/// the fingerprint recorded at controller startup). If the active config is
/// then switched away from the failing one and back to it (or to any other
/// config whose fingerprint happens to match that stale value), the
/// latch-clearing checks in the main loop would see no change and the
/// permanent latch would never clear — leaving CamillaDSP stuck offline even
/// after the user reselects a config that is known to work.
///
/// By re-sampling here, the latch is guaranteed to clear as soon as
/// `adapt_path` differs from the config that just failed, regardless of what
/// it looked like before the failure.
fn latch_on_config_error(
    retry: &mut RetryState,
    last_fingerprint: &mut ConfigFingerprint,
    adapt_path: &PathBuf,
) {
    *last_fingerprint = ConfigFingerprint::sample(adapt_path);
    retry.latch();
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
///    delay; the current policy latches an immediate exit as a config/device
///    failure until the baseline config changes.
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
                    latch_on_config_error(&mut retry, &mut last_fingerprint, &adapt_path);
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
                latch_on_config_error(&mut retry, &mut last_fingerprint, &adapt_path);
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
            latch_on_config_error(&mut retry, &mut last_fingerprint, &adapt_path);
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

    /// Atomically retarget a symlink, mirroring how CamillaGUI switches the
    /// active config (`ln -sfn` via a temp symlink + rename in production).
    fn retarget_symlink(link: &Path, target: &Path) {
        let tmp = link.with_extension("tmp-retarget");
        let _ = fs::remove_file(&tmp);
        symlink(target, &tmp).unwrap();
        fs::rename(&tmp, link).unwrap();
    }

    // ── latch_on_config_error / fingerprint interaction ────────────────────

    #[test]
    fn latch_on_config_error_latches_retry_state() {
        let dir = test_dir("latch-basic");
        let config = dir.join("MyDSP.yml");
        fs::write(&config, "devices:\n  samplerate: 44100\n").unwrap();

        let mut retry = RetryState::new();
        let mut last_fingerprint = ConfigFingerprint::absent();

        latch_on_config_error(&mut retry, &mut last_fingerprint, &config);

        assert!(retry.latch_until_change);
        assert!(!retry.should_attempt());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn latch_on_config_error_captures_the_failing_configs_fingerprint() {
        let dir = test_dir("latch-fingerprint");
        let config = dir.join("Bad.yml");
        fs::write(&config, "devices:\n  samplerate: 44100\n").unwrap();

        let mut retry = RetryState::new();
        // Simulate a stale fingerprint left over from controller startup,
        // sampled long before the failing config became active.
        let mut last_fingerprint = ConfigFingerprint::absent();

        latch_on_config_error(&mut retry, &mut last_fingerprint, &config);

        // The fingerprint must now reflect the config that just failed, not
        // the stale startup value.
        assert_eq!(last_fingerprint, ConfigFingerprint::sample(&config));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn latch_persists_while_the_failing_config_remains_active() {
        let dir = test_dir("latch-persists");
        let bad = dir.join("Bad.yml");
        let active = dir.join("active_config.yml");
        fs::write(&bad, "devices:\n  samplerate: 44100\n").unwrap();
        symlink(&bad, &active).unwrap();

        let mut retry = RetryState::new();
        let mut last_fingerprint = ConfigFingerprint::sample(&active);

        latch_on_config_error(&mut retry, &mut last_fingerprint, &active);
        assert!(!retry.should_attempt());

        // No config switch happened: the fingerprint must still match, so a
        // latch-clearing check would (correctly) leave the latch in place.
        let new_fp = ConfigFingerprint::sample(&active);
        assert_eq!(new_fp, last_fingerprint);
        assert!(!retry.should_attempt());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reselecting_a_previously_working_config_clears_the_latch() {
        // Regression test for the "CamillaGUI status stuck offline" bug:
        // switching to a bad config latches the retry state, and switching
        // back to a config that worked earlier (e.g. right after reboot)
        // must clear the latch rather than being mistaken for "no change"
        // against a stale fingerprint.
        let dir = test_dir("latch-reselect");
        let good = dir.join("Good.yml");
        let bad = dir.join("Bad.yml");
        let active = dir.join("active_config.yml");
        fs::write(&good, "devices:\n  samplerate: 44100\n").unwrap();
        fs::write(&bad, "devices:\n  samplerate: 48000\n").unwrap();
        symlink(&good, &active).unwrap();

        // Controller startup: baseline fingerprint captured once, well before
        // any failure occurs.
        let mut retry = RetryState::new();
        let mut last_fingerprint = ConfigFingerprint::sample(&active);

        // CamillaGUI switches to the bad config; the switch attempt fails
        // (adapt/startup error) and the controller latches.
        retarget_symlink(&active, &bad);
        latch_on_config_error(&mut retry, &mut last_fingerprint, &active);
        assert!(!retry.should_attempt(), "latch must be set after failure");

        // User reselects the config that worked after reboot.
        retarget_symlink(&active, &good);

        // The main-loop latch-clearing check: sample the current fingerprint
        // and compare against the one captured at latch time.
        let new_fp = ConfigFingerprint::sample(&active);
        assert_ne!(
            new_fp, last_fingerprint,
            "reselecting a working config must be detected as a change"
        );

        // Apply the same reset the controller loop performs on a detected
        // change, and confirm the latch is cleared.
        last_fingerprint = new_fp;
        retry.reset();
        assert!(retry.should_attempt(), "latch must clear once cleared");
        assert!(!retry.latch_until_change);
        assert_eq!(last_fingerprint, ConfigFingerprint::sample(&active));

        fs::remove_dir_all(dir).unwrap();
    }
}
