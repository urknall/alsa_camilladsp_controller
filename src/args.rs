use crate::error::{app_error, AppResult};
use crate::logging::LogLevel;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ─── Structs ───────────────────────────────────────────────────────────────

/// Parsed command-line arguments.
#[derive(Clone, Debug)]
pub struct Args {
    pub host: String,
    pub port: u16,
    pub device: String,
    pub adapt: Option<PathBuf>,
    pub initial_rate: Option<u32>,
    pub initial_format: Option<String>,
    pub initial_channels: Option<u32>,
    pub log_level: LogLevel,
    pub mode: Mode,
    /// Stream-detection / PCM-transport backend.
    pub backend: Backend,
    /// AF_UNIX socket path for the ioplug IPC channel.
    pub socket_path: Option<PathBuf>,
    /// Path to the `camilladsp` binary (required with `--backend ioplug`).
    pub camilladsp_binary: Option<PathBuf>,
    /// CamillaDSP YAML config/statefile path supplied to `--get-playback-device`/`--get-config-path`.
    pub config_path: Option<PathBuf>,
    /// Playback device string supplied to `--make-bypass`.
    pub playback_device: Option<String>,
    /// Output file path for writer/report modes.
    pub output: Option<PathBuf>,
    /// Config path value written into the statefile by `--make-statefile`.
    pub statefile_config_path: Option<String>,
    /// Path to the existing statefile to read mute/volume from (`--existing-state`).
    pub existing_state: Option<PathBuf>,
    /// Benchmark plan path used by `--validate-benchmark-plan`.
    pub benchmark_path: Option<PathBuf>,
    /// Path to the CamillaDSP statefile, forwarded as `--statefile` when
    /// spawning CamillaDSP in ioplug mode (preserves volume/mute across
    /// streams and makes the WebSocket discoverable by CamillaGUI).
    pub cdsp_statefile: Option<PathBuf>,
}

/// Stream-detection and PCM-transport backend.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// snd-aloop loopback device (stable, default).
    Aloop,
    /// ALSA ioplug direct-connect (experimental).
    Ioplug,
}

/// Operating mode selected by the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Normal ALSA-loopback control loop.
    Run,
    /// Read snd-aloop HCTL controls once and exit (install-time probe).
    Probe,
    /// Connect to CamillaDSP, query GetVersion, close, exit.
    WsCheck,
    /// Adapt YAML and call ValidateConfig over WebSocket, exit.
    WsValidate,
    /// Adapt YAML once, write result to stdout, exit.
    AdaptCheck,
    /// Read `devices.playback.device` from a CamillaDSP YAML config and print it.
    GetPlaybackDevice,
    /// Read `config_path` from a CamillaDSP statefile and print it.
    GetConfigPath,
    /// Read validated `mute`/`volume` blocks from a CamillaDSP statefile and print them.
    GetStateFragment,
    /// Write a piCoreDSP bypass CamillaDSP config with the given playback device.
    MakeBypass,
    /// Query `GetConfigFilePath` over WebSocket and print the result.
    WsGetConfigPath,
    /// Write a CamillaDSP statefile (first install or reinstall preserving mute/volume).
    MakeStatefile,
    /// Write a benchmark plan template for A/B backend measurements.
    MakeBenchmarkPlan,
    /// Validate a benchmark plan before collecting measurements.
    ValidateBenchmarkPlan,
    /// Render a benchmark report from a benchmark plan.
    MakeBenchmarkReport,
    /// Automatically collect metrics for both backends and write a populated benchmark plan.
    RunBenchmark,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Run => "--run (default)",
            Self::Probe => "--probe",
            Self::WsCheck => "--ws-check",
            Self::WsValidate => "--ws-validate",
            Self::AdaptCheck => "--adapt-check",
            Self::GetPlaybackDevice => "--get-playback-device",
            Self::GetConfigPath => "--get-config-path",
            Self::GetStateFragment => "--get-state-fragment",
            Self::MakeBypass => "--make-bypass",
            Self::WsGetConfigPath => "--ws-get-config-path",
            Self::MakeStatefile => "--make-statefile",
            Self::MakeBenchmarkPlan => "--make-benchmark-plan",
            Self::ValidateBenchmarkPlan => "--validate-benchmark-plan",
            Self::MakeBenchmarkReport => "--make-benchmark-report",
            Self::RunBenchmark => "--run-benchmark",
        }
    }
}

impl Default for Args {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 1234,
            device: "hw:Loopback,0".to_owned(),
            adapt: None,
            initial_rate: None,
            initial_format: None,
            initial_channels: None,
            log_level: LogLevel::Info,
            mode: Mode::Run,
            config_path: None,
            playback_device: None,
            output: None,
            statefile_config_path: None,
            existing_state: None,
            benchmark_path: None,
            cdsp_statefile: None,
            backend: Backend::Aloop,
            socket_path: None,
            camilladsp_binary: None,
        }
    }
}

// ─── Usage ─────────────────────────────────────────────────────────────────

pub fn usage() {
    println!(
        "picoredsp-controller {VERSION}\n\
Rust ALSA loopback controller for CamillaDSP\n\n\
Usage:\n\
  picoredsp-controller --adapt PATH [options]\n\
  picoredsp-controller --probe [--device DEVICE]\n\
  picoredsp-controller --ws-check [--host HOST] [--port PORT]\n\
  picoredsp-controller --ws-validate --adapt PATH [--host HOST] [--port PORT]\n\
  picoredsp-controller --adapt-check --adapt PATH [--rate R --format F --channels N]\n\
  picoredsp-controller --get-playback-device CONFIG\n\
  picoredsp-controller --get-config-path STATEFILE\n\
  picoredsp-controller --get-state-fragment STATEFILE\n\
  picoredsp-controller --make-bypass --playback-device DEVICE [--output FILE]\n\
  picoredsp-controller --ws-get-config-path [--host HOST] [--port PORT]\n\
  picoredsp-controller --make-statefile --config-path PATH --output FILE [--existing-state OLD]\n\
  picoredsp-controller --make-benchmark-plan [--output FILE]\n\
  picoredsp-controller --validate-benchmark-plan FILE\n\
  picoredsp-controller --make-benchmark-report FILE [--output FILE]\n\
  picoredsp-controller --run-benchmark [--host HOST] [--port PORT] [--device DEVICE] [--output FILE]\n\n\
Options:\n\
  -a, --adapt PATH              Active config path/symlink to adapt\n\
  -d, --device DEVICE           ALSA control device (default: hw:Loopback,0)\n\
      --host HOST               CamillaDSP websocket host (default: 127.0.0.1)\n\
  -p, --port PORT               CamillaDSP websocket port (default: 1234)\n\
  -r, --rate RATE               Initial fallback sample rate\n\
  -f, --format FORMAT           Initial fallback CamillaDSP sample format (e.g. S32_LE)\n\
  -c, --channels N              Initial fallback capture channel count\n\
  -l, --log-level LEVEL         DEBUG, INFO, WARNING, ERROR, CRITICAL\n\
      --probe                   Read snd-aloop controls once and exit\n\
      --ws-check                Connect, query CamillaDSP version, close, exit\n\
      --ws-validate             Adapt YAML and ValidateConfig over websocket\n\
      --adapt-check             Adapt YAML once, write result to stdout, exit\n\
      --get-playback-device PATH  Print devices.playback.device from a YAML config\n\
      --get-config-path STATEFILE Print config_path from a CamillaDSP statefile\n\
      --get-state-fragment STATEFILE  Print validated mute/volume YAML from a CamillaDSP statefile\n\
      --make-bypass             Write a piCoreDSP bypass CamillaDSP config\n\
      --playback-device DEVICE  Playback device for --make-bypass\n\
      --output FILE             Output file for --make-bypass, --make-statefile, --make-benchmark-plan, or --make-benchmark-report\n\
      --ws-get-config-path      Query GetConfigFilePath from CamillaDSP via WebSocket\n\
      --make-statefile          Write a CamillaDSP statefile (first install or reinstall)\n\
      --config-path PATH        config_path value to embed in the new statefile (--make-statefile)\n\
      --existing-state FILE     Existing statefile to preserve mute/volume from (--make-statefile)\n\
      --make-benchmark-plan     Write an A/B benchmark plan template\n\
      --validate-benchmark-plan FILE  Validate an A/B benchmark plan YAML file\n\
      --make-benchmark-report FILE  Render a benchmark report from a benchmark plan\n\
      --run-benchmark           Auto-collect metrics for both backends and write populated plan\n\
  -h, --help                    Show this help\n\
  -V, --version                 Show version\n\
      --backend BACKEND         Stream backend: aloop (default) or ioplug\n\
      --socket-path PATH        AF_UNIX socket path for ioplug IPC (required with --backend ioplug)\n\
      --camilladsp PATH         Path to camilladsp binary (required with --backend ioplug)\n\
      --cdsp-statefile PATH     CamillaDSP statefile forwarded as --statefile on spawn (ioplug only)"
   );
}

// ─── Argument parser ───────────────────────────────────────────────────────

/// Parse `std::env::args()`, returning `Ok(None)` when `--help` or `--version`
/// consumed the arguments and `Ok(Some(args))` otherwise.
pub fn parse_args() -> AppResult<Option<Args>> {
    parse_args_from(env::args().skip(1))
}

fn parse_args_from<I>(iterable: I) -> AppResult<Option<Args>>
where
    I: IntoIterator<Item = String>,
{
    let mut args = Args::default();
    let mut iter = iterable.into_iter();

    while let Some(arg) = iter.next() {
        let mut next_value = |name: &str| -> AppResult<String> {
            iter.next()
                .ok_or_else(|| app_error(format!("{name} requires a value")))
        };

        match arg.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("picoredsp-controller {VERSION}");
                return Ok(None);
            }
            "--probe" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --probe",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::Probe;
            }
            "--ws-check" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --ws-check",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::WsCheck;
            }
            "--ws-validate" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --ws-validate",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::WsValidate;
            }
            "--adapt-check" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --adapt-check",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::AdaptCheck;
            }
            "--get-playback-device" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --get-playback-device",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::GetPlaybackDevice;
                args.config_path = Some(PathBuf::from(next_value("--get-playback-device")?));
            }
            "--get-config-path" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --get-config-path",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::GetConfigPath;
                args.config_path = Some(PathBuf::from(next_value("--get-config-path")?));
            }
            "--get-state-fragment" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --get-state-fragment",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::GetStateFragment;
                args.config_path = Some(PathBuf::from(next_value("--get-state-fragment")?));
            }
            "--make-bypass" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --make-bypass",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::MakeBypass;
            }
            "--ws-get-config-path" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --ws-get-config-path",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::WsGetConfigPath;
            }
            "--make-statefile" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --make-statefile",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::MakeStatefile;
            }
            "--make-benchmark-plan" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --make-benchmark-plan",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::MakeBenchmarkPlan;
            }
            "--validate-benchmark-plan" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --validate-benchmark-plan",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::ValidateBenchmarkPlan;
                args.benchmark_path = Some(PathBuf::from(next_value("--validate-benchmark-plan")?));
            }
            "--make-benchmark-report" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --make-benchmark-report",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::MakeBenchmarkReport;
                args.benchmark_path = Some(PathBuf::from(next_value("--make-benchmark-report")?));
            }
            "--run-benchmark" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --run-benchmark",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::RunBenchmark;
            }
            "--config-path" => {
                args.statefile_config_path = Some(next_value("--config-path")?);
            }
            "--existing-state" => {
                args.existing_state = Some(PathBuf::from(next_value("--existing-state")?));
            }
            "--playback-device" => {
                args.playback_device = Some(next_value("--playback-device")?);
            }
            "--output" => {
                args.output = Some(PathBuf::from(next_value("--output")?));
            }
            "--host" => args.host = next_value("--host")?,
            "-p" | "--port" => {
                let v: u16 = next_value("--port")?
                    .parse()
                    .map_err(|_| app_error("--port must be an integer from 1 to 65535"))?;
                if v == 0 {
                    return Err(app_error("--port must be an integer from 1 to 65535"));
                }
                args.port = v;
            }
            "-d" | "--device" => args.device = next_value("--device")?,
            "-a" | "--adapt" => args.adapt = Some(PathBuf::from(next_value("--adapt")?)),
            "-r" | "--rate" => {
                let v: u32 = next_value("--rate")?
                    .parse()
                    .map_err(|_| app_error("--rate must be a positive integer"))?;
                if v == 0 {
                    return Err(app_error("--rate must be a positive integer"));
                }
                args.initial_rate = Some(v);
            }
            "-f" | "--format" => args.initial_format = Some(next_value("--format")?),
            "-c" | "--channels" => {
                let v: u32 = next_value("--channels")?
                    .parse()
                    .map_err(|_| app_error("--channels must be a positive integer"))?;
                if v == 0 {
                    return Err(app_error("--channels must be a positive integer"));
                }
                args.initial_channels = Some(v);
            }
            "-l" | "--log-level" => {
                args.log_level = LogLevel::parse(&next_value("--log-level")?)?;
            }
            "--backend" => {
                let v = next_value("--backend")?;
                args.backend = match v.as_str() {
                    "aloop" => Backend::Aloop,
                    "ioplug" => Backend::Ioplug,
                    other => {
                        return Err(app_error(format!(
                            "--backend must be 'aloop' or 'ioplug', got '{other}'"
                        )))
                    }
                };
            }
            "--socket-path" => {
                args.socket_path = Some(PathBuf::from(next_value("--socket-path")?));
            }
            "--camilladsp" => {
                args.camilladsp_binary = Some(PathBuf::from(next_value("--camilladsp")?));
            }
            "--cdsp-statefile" => {
                args.cdsp_statefile = Some(PathBuf::from(next_value("--cdsp-statefile")?));
            }
            other => return Err(app_error(format!("unknown argument: {other}"))),
        }
    }

    if matches!(args.mode, Mode::Run | Mode::WsValidate | Mode::AdaptCheck) && args.adapt.is_none()
    {
        return Err(app_error("this mode requires --adapt PATH"));
    }
    if args.mode == Mode::MakeBypass && args.playback_device.is_none() {
        return Err(app_error("--make-bypass requires --playback-device DEVICE"));
    }
    if args.playback_device.is_some() && args.mode != Mode::MakeBypass {
        return Err(app_error(
            "--playback-device is only valid with --make-bypass",
        ));
    }
    if args.output.is_some()
        && !matches!(
            args.mode,
            Mode::MakeBypass
                | Mode::MakeStatefile
                | Mode::MakeBenchmarkPlan
                | Mode::MakeBenchmarkReport
                | Mode::RunBenchmark
        )
    {
        return Err(app_error(
            "--output is only valid with --make-bypass, --make-statefile, --make-benchmark-plan, --make-benchmark-report or --run-benchmark",
        ));
    }
    if args.mode == Mode::MakeStatefile {
        if args.statefile_config_path.is_none() {
            return Err(app_error("--make-statefile requires --config-path PATH"));
        }
        if args.output.is_none() {
            return Err(app_error("--make-statefile requires --output FILE"));
        }
    }
    if args.statefile_config_path.is_some() && args.mode != Mode::MakeStatefile {
        return Err(app_error(
            "--config-path is only valid with --make-statefile",
        ));
    }
    if args.existing_state.is_some() && args.mode != Mode::MakeStatefile {
        return Err(app_error(
            "--existing-state is only valid with --make-statefile",
        ));
    }
    if args.benchmark_path.is_some()
        && !matches!(
            args.mode,
            Mode::ValidateBenchmarkPlan | Mode::MakeBenchmarkReport
        )
    {
        return Err(app_error(
            "--benchmark-path is only valid with --validate-benchmark-plan or --make-benchmark-report",
        ));
    }
    if args.mode == Mode::MakeStatefile {
        if args.adapt.is_some() {
            return Err(app_error("--adapt is not valid with --make-statefile"));
        }
        if args.initial_rate.is_some() {
            return Err(app_error("--rate is not valid with --make-statefile"));
        }
        if args.initial_format.is_some() {
            return Err(app_error("--format is not valid with --make-statefile"));
        }
        if args.initial_channels.is_some() {
            return Err(app_error("--channels is not valid with --make-statefile"));
        }
    }
    if matches!(
        args.mode,
        Mode::MakeBenchmarkPlan
            | Mode::ValidateBenchmarkPlan
            | Mode::MakeBenchmarkReport
            | Mode::RunBenchmark
    ) {
        if args.adapt.is_some() {
            return Err(app_error(format!(
                "--adapt is not valid with {}",
                args.mode.name()
            )));
        }
        if args.initial_rate.is_some() {
            return Err(app_error(format!(
                "--rate is not valid with {}",
                args.mode.name()
            )));
        }
        if args.initial_format.is_some() {
            return Err(app_error(format!(
                "--format is not valid with {}",
                args.mode.name()
            )));
        }
        if args.initial_channels.is_some() {
            return Err(app_error(format!(
                "--channels is not valid with {}",
                args.mode.name()
            )));
        }
        if args.playback_device.is_some() {
            return Err(app_error(format!(
                "--playback-device is not valid with {}",
                args.mode.name()
            )));
        }
        if args.statefile_config_path.is_some() {
            return Err(app_error(format!(
                "--config-path is not valid with {}",
                args.mode.name()
            )));
        }
        if args.existing_state.is_some() {
            return Err(app_error(format!(
                "--existing-state is not valid with {}",
                args.mode.name()
            )));
        }
    }
    if matches!(
        args.mode,
        Mode::GetPlaybackDevice | Mode::GetConfigPath | Mode::GetStateFragment
    ) {
        if args.adapt.is_some() {
            return Err(app_error(format!(
                "--adapt is not valid with {}",
                args.mode.name()
            )));
        }
        if args.initial_rate.is_some() {
            return Err(app_error(format!(
                "--rate is not valid with {}",
                args.mode.name()
            )));
        }
        if args.initial_format.is_some() {
            return Err(app_error(format!(
                "--format is not valid with {}",
                args.mode.name()
            )));
        }
        if args.initial_channels.is_some() {
            return Err(app_error(format!(
                "--channels is not valid with {}",
                args.mode.name()
            )));
        }
    }
    Ok(Some(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> AppResult<Option<Args>> {
        parse_args_from(argv.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parse_make_benchmark_plan_mode() {
        let args = parse(&["--make-benchmark-plan", "--output", "plan.yml"])
            .expect("parse ok")
            .expect("args");
        assert_eq!(args.mode, Mode::MakeBenchmarkPlan);
        assert_eq!(args.output, Some(PathBuf::from("plan.yml")));
    }

    #[test]
    fn parse_validate_benchmark_plan_mode() {
        let args = parse(&["--validate-benchmark-plan", "plan.yml"])
            .expect("parse ok")
            .expect("args");
        assert_eq!(args.mode, Mode::ValidateBenchmarkPlan);
        assert_eq!(args.benchmark_path, Some(PathBuf::from("plan.yml")));
    }

    #[test]
    fn parse_make_benchmark_report_mode() {
        let args = parse(&[
            "--make-benchmark-report",
            "plan.yml",
            "--output",
            "report.md",
        ])
        .expect("parse ok")
        .expect("args");
        assert_eq!(args.mode, Mode::MakeBenchmarkReport);
        assert_eq!(args.benchmark_path, Some(PathBuf::from("plan.yml")));
        assert_eq!(args.output, Some(PathBuf::from("report.md")));
    }

    #[test]
    fn reject_output_with_validate_benchmark_plan() {
        let err = parse(&[
            "--validate-benchmark-plan",
            "plan.yml",
            "--output",
            "ignored.yml",
        ])
        .expect_err("output must be rejected");
        assert!(
            err.to_string()
                .contains("--output is only valid with --make-bypass, --make-statefile, --make-benchmark-plan, --make-benchmark-report or --run-benchmark"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_run_benchmark_mode() {
        let args = parse(&["--run-benchmark"])
            .expect("parse ok")
            .expect("args");
        assert_eq!(args.mode, Mode::RunBenchmark);
        assert_eq!(args.output, None);
    }

    #[test]
    fn parse_run_benchmark_mode_with_output() {
        let args = parse(&["--run-benchmark", "--output", "plan.yml"])
            .expect("parse ok")
            .expect("args");
        assert_eq!(args.mode, Mode::RunBenchmark);
        assert_eq!(args.output, Some(PathBuf::from("plan.yml")));
    }

    #[test]
    fn parse_run_benchmark_mode_with_host_port_device() {
        let args = parse(&[
            "--run-benchmark",
            "--host",
            "192.168.1.1",
            "--port",
            "5678",
            "--device",
            "hw:Loopback,0",
        ])
        .expect("parse ok")
        .expect("args");
        assert_eq!(args.mode, Mode::RunBenchmark);
        assert_eq!(args.host, "192.168.1.1");
        assert_eq!(args.port, 5678);
        assert_eq!(args.device, "hw:Loopback,0");
    }

    #[test]
    fn run_benchmark_rejects_conflicting_mode_flag() {
        let err = parse(&["--run-benchmark", "--probe"]).expect_err("conflicting modes must fail");
        assert!(
            err.to_string().contains("conflicting mode flags"),
            "unexpected error: {err}"
        );
    }
}
