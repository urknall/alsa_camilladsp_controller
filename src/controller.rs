use crate::args::Args;
use crate::backend::aloop::AloopBackend;
use crate::camilladsp::alsa_capture::AlsaLoopbackListener;
use crate::camilladsp::websocket::CamillaWs;
use crate::core::config::{DeviceSnapshot, WaveFormat};
use crate::core::errors::{app_error, AppResult};
pub use crate::core::state_machine::Controller;

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
