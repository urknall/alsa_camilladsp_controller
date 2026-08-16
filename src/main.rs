//! piCoreCDSP v2 binary entry point.
//!
//! # Configuration (environment variables)
//!
//! | Variable                  | Default                    | Description                                      |
//! |---------------------------|----------------------------|--------------------------------------------------|
//! | `PICORECDSP_CAMILLA_URL`  | `ws://127.0.0.1:1234`      | CamillaDSP WebSocket URL                         |
//! | `PICORECDSP_LOG`          | `info`                     | Log level (error/warn/info/debug/trace)          |
//! | `RUST_LOG`                | —                          | Fallback log level (standard env_logger syntax)  |

use std::env;

use picorecdsp::{
    camilla::protocol_v4::CamillaDspV4,
    logging,
    rate_sync::{ConfigPatchRateSynchronizer, PollingTrigger},
    reconcile::{run_loop, ReconcileConfig},
    source::alsa_loopback::AlsaLoopbackObserver,
};

#[tokio::main]
async fn main() {
    logging::init_logging();

    let camilla_url =
        env::var("PICORECDSP_CAMILLA_URL").unwrap_or_else(|_| "ws://127.0.0.1:1234".to_string());

    log::info!("piCoreCDSP v2 starting — connecting to CamillaDSP at {camilla_url}");

    let camilla = CamillaDspV4::new(&camilla_url);
    let rate_sync = ConfigPatchRateSynchronizer::new(&camilla);
    let cfg = ReconcileConfig::default();

    // Use a polling trigger (PollingTrigger at the safety-reconcile interval).
    // On CamillaDSP 4.2+ the CamillaDspV4StateEvents subscriber can be wired in
    // here instead for faster response; the polling fallback is always safe.
    let mut trigger = PollingTrigger::new(cfg.safety_reconcile_interval);

    // ── Source observer (Linux: read /proc/asound/; other: compile error) ──────
    // On a real pCP target this is always Linux.  The cfg guard prevents the
    // binary from being accidentally run on non-Linux targets where /proc is
    // unavailable.
    #[cfg(not(target_os = "linux"))]
    compile_error!(
        "picorecdsp requires Linux — the AlsaLoopbackObserver reads /proc/asound/ \
         which is only available on Linux targets."
    );

    #[cfg(target_os = "linux")]
    let source_observer = AlsaLoopbackObserver::new_default();

    log::info!("piCoreCDSP v2 reconcile loop starting");

    #[cfg(target_os = "linux")]
    let result = run_loop(&camilla, &source_observer, &rate_sync, &mut trigger, &cfg).await;

    match result {
        Ok(infallible) => match infallible {},
        Err(e) => {
            log::error!("piCoreCDSP fatal error: {e}");
            std::process::exit(1);
        }
    }
}
