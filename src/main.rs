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

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAction {
    Run,
    PrintHelp,
    PrintVersion,
}

fn usage(program: &str) -> String {
    format!(
        "\
Usage: {program} [--help] [--version]

piCoreCDSP v2 runtime daemon.

Environment:
  PICORECDSP_CAMILLA_URL   CamillaDSP WebSocket URL (default: ws://127.0.0.1:1234)
  PICORECDSP_LOG           Log level (default: info)
  RUST_LOG                 Fallback log filter"
    )
}

fn parse_cli_action<I>(args: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let program = args.next().unwrap_or_else(|| "picorecdsp".to_string());

    match (args.next(), args.next()) {
        (None, None) => Ok(CliAction::Run),
        (Some(flag), None) if flag == "-h" || flag == "--help" => Ok(CliAction::PrintHelp),
        (Some(flag), None) if flag == "-V" || flag == "--version" => Ok(CliAction::PrintVersion),
        (Some(flag), None) => Err(format!(
            "ERROR: Unknown option: {flag}\n\n{}",
            usage(&program)
        )),
        (Some(flag), Some(_)) if flag == "-h" || flag == "--help" || flag == "-V" || flag == "--version" => {
            Err(format!("ERROR: {flag} does not accept additional arguments.\n\n{}", usage(&program)))
        }
        (Some(flag), Some(_)) => Err(format!(
            "ERROR: Unknown option: {flag}\n\n{}",
            usage(&program)
        )),
    }
}

#[tokio::main]
async fn main() {
    match parse_cli_action(env::args()) {
        Ok(CliAction::PrintHelp) => {
            println!("{}", usage("picorecdsp"));
            return;
        }
        Ok(CliAction::PrintVersion) => {
            println!("picorecdsp {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Ok(CliAction::Run) => {}
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }

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

    #[cfg(target_os = "linux")]
    match result {
        Ok(infallible) => match infallible {},
        Err(e) => {
            log::error!("piCoreCDSP fatal error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_cli_action, CliAction};

    #[test]
    fn no_arguments_runs_daemon() {
        let action = parse_cli_action(["picorecdsp".to_string()]).unwrap();
        assert_eq!(action, CliAction::Run);
    }

    #[test]
    fn help_argument_prints_help_instead_of_running() {
        let action = parse_cli_action(["picorecdsp".to_string(), "--help".to_string()]).unwrap();
        assert_eq!(action, CliAction::PrintHelp);
    }

    #[test]
    fn version_argument_prints_version_instead_of_running() {
        let action = parse_cli_action(["picorecdsp".to_string(), "--version".to_string()]).unwrap();
        assert_eq!(action, CliAction::PrintVersion);
    }

    #[test]
    fn unknown_argument_is_rejected() {
        let error = parse_cli_action(["picorecdsp".to_string(), "--bogus".to_string()]).unwrap_err();
        assert!(error.contains("Unknown option"));
    }

    #[test]
    fn help_with_extra_arguments_is_rejected() {
        let error =
            parse_cli_action(["picorecdsp".to_string(), "--help".to_string(), "extra".to_string()]).unwrap_err();
        assert!(error.contains("does not accept additional arguments"));
    }
}
