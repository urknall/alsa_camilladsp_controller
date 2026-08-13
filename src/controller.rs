use crate::args::Args;
use crate::backend::aloop::AloopBackend;
use crate::backend::ioplug::IoplugBackend;
use crate::camilladsp::alsa_capture::AlsaLoopbackListener;
use crate::camilladsp::supervisor::StdinSupervisor;
use crate::camilladsp::websocket::CamillaWs;
use crate::core::adaptation::adapt_config_for_backend;
use crate::core::adaptation::RuntimeBackend;
use crate::core::config::{DeviceSnapshot, WaveFormat};
use crate::core::errors::{app_error, AppResult};
use crate::core::logging::{log, LogLevel};
pub use crate::core::state_machine::Controller;
use crate::ipc::protocol::ErrorCode;

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

/// Run the ioplug controller loop (Gate 8).
///
/// For each stream:
/// 1. Wait for the plugin to connect and send START.
/// 2. Adapt the CamillaDSP config for the negotiated stream parameters.
/// 3. Spawn CamillaDSP with the pipe read-end as stdin.
/// 4. Send READY to the plugin, delivering the pipe write-end via SCM_RIGHTS.
/// 5. Monitor the stream; wait for STOP or plugin disconnect.
/// 6. Close our copy of the pipe write-end → CamillaDSP sees EOF → exits.
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

    let mut backend = IoplugBackend::new(&socket_path, log_level)?;
    let mut supervisor = StdinSupervisor::new(&camilladsp_binary, &adapt_path, log_level);

    log(
        LogLevel::Info,
        log_level,
        format!(
            "ioplug controller started; socket={} camilladsp={}",
            socket_path.display(),
            camilladsp_binary.display(),
        ),
    );

    loop {
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
                _ => continue,
            }
        };

        log(
            LogLevel::Info,
            log_level,
            format!(
                "ioplug: adapting config for rate={} format={} channels={}",
                wave.sample_rate.unwrap_or(0),
                wave.sample_format.as_deref().unwrap_or("?"),
                wave.channels.unwrap_or(0),
            ),
        );

        // ── Adapt the baseline config ──────────────────────────────────
        match adapt_config_for_backend(&adapt_path, &wave, RuntimeBackend::Ioplug) {
            Ok(_adapted) => {}
            Err(err) => {
                log(
                    LogLevel::Error,
                    log_level,
                    format!("ioplug: config adaptation failed: {err}"),
                );
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
                backend.send_error_to_plugin(ErrorCode::Internal);
                continue;
            }
        };

        // ── Send READY + pipe write-end to the plugin ─────────────────
        if let Err(err) = backend.send_ready_with_fd_to_plugin(pipe_write_fd) {
            log(
                LogLevel::Error,
                log_level,
                format!("ioplug: failed to send READY+fd: {err}"),
            );
            supervisor.stop_stream();
            continue;
        }

        log(
            LogLevel::Info,
            log_level,
            "ioplug: stream active — waiting for STOP",
        );

        // ── Monitor stream health and wait for STOP ────────────────────
        loop {
            use crate::backend::ControllerBackend;

            // Check if CamillaDSP died unexpectedly.
            if !supervisor.is_running() {
                log(
                    LogLevel::Error,
                    log_level,
                    "ioplug: CamillaDSP exited unexpectedly",
                );
                // Drop back to Idle; the plugin will get a write error on its
                // pipe fd and should report EPIPE to the ALSA layer.
                break;
            }

            match backend.poll_event(200)? {
                Some(crate::backend::StreamEvent::Stopped) => break,
                _ => continue,
            }
        }

        log(LogLevel::Info, log_level, "ioplug: stream stopped");

        // ── Shut down CamillaDSP ───────────────────────────────────────
        // Closing our write-end sends EOF once the plugin also closes its copy.
        supervisor.stop_stream();
    }
}
