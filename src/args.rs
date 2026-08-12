use crate::error::{app_error, AppResult};
use crate::logging::LogLevel;
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
    /// CamillaDSP YAML config/statefile path supplied to `--get-playback-device`/`--get-config-path`.
    pub config_path: Option<PathBuf>,
    /// Playback device string supplied to `--make-bypass`.
    pub playback_device: Option<String>,
    /// Output file path for `--make-bypass` and `--make-statefile` (stdout when absent for bypass).
    pub output: Option<PathBuf>,
    /// Config path value written into the statefile by `--make-statefile`.
    pub statefile_config_path: Option<String>,
    /// Path to the existing statefile to read mute/volume from (`--existing-state`).
    pub existing_state: Option<PathBuf>,
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
    /// Adapt YAML once and send it to CamillaDSP via `SetConfig`, exit.
    WsApply,
    /// Write a CamillaDSP statefile (first install or reinstall preserving mute/volume).
    MakeStatefile,
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
            Self::WsApply => "--ws-apply",
            Self::MakeStatefile => "--make-statefile",
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
  picoredsp-controller --ws-apply --adapt PATH [--rate R --format F --channels N] [--host HOST] [--port PORT]\n\
  picoredsp-controller --make-statefile --config-path PATH --output FILE [--existing-state OLD]\n\n\
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
      --output FILE             Output file for --make-bypass (default: stdout) or --make-statefile\n\
      --ws-get-config-path      Query GetConfigFilePath from CamillaDSP via WebSocket\n\
      --ws-apply                Adapt YAML and send it to CamillaDSP via SetConfig, then exit\n\
      --make-statefile          Write a CamillaDSP statefile (first install or reinstall)\n\
      --config-path PATH        config_path value to embed in the new statefile (--make-statefile)\n\
      --existing-state FILE     Existing statefile to preserve mute/volume from (--make-statefile)\n\
  -h, --help                    Show this help\n\
  -V, --version                 Show version"
    );
}

// ─── Argument parser ───────────────────────────────────────────────────────

/// Parse `std::env::args()`, returning `Ok(None)` when `--help` or `--version`
/// consumed the arguments and `Ok(Some(args))` otherwise.
pub fn parse_args() -> AppResult<Option<Args>> {
    let mut args = Args::default();
    let mut iter = env::args().skip(1);

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
            "--ws-apply" => {
                if args.mode != Mode::Run {
                    return Err(app_error(format!(
                        "conflicting mode flags: {} and --ws-apply",
                        args.mode.name()
                    )));
                }
                args.mode = Mode::WsApply;
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
            other => return Err(app_error(format!("unknown argument: {other}"))),
        }
    }

    if matches!(
        args.mode,
        Mode::Run | Mode::WsValidate | Mode::AdaptCheck | Mode::WsApply
    ) && args.adapt.is_none()
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
    if args.output.is_some() && !matches!(args.mode, Mode::MakeBypass | Mode::MakeStatefile) {
        return Err(app_error(
            "--output is only valid with --make-bypass or --make-statefile",
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
