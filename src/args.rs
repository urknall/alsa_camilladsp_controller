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
}

impl Default for Args {
    fn default() -> Self {
        Self {
            host: "localhost".to_owned(),
            port: 1234,
            device: "hw:Loopback,0".to_owned(),
            adapt: None,
            initial_rate: None,
            initial_format: None,
            initial_channels: None,
            log_level: LogLevel::Info,
            mode: Mode::Run,
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
  picoredsp-controller --adapt-check --adapt PATH [--rate R --format F --channels N]\n\n\
Options:\n\
  -a, --adapt PATH       Active config path/symlink to adapt\n\
  -d, --device DEVICE    ALSA control device (default: hw:Loopback,0)\n\
      --host HOST        CamillaDSP websocket host (default: localhost)\n\
  -p, --port PORT        CamillaDSP websocket port (default: 1234)\n\
  -r, --rate RATE        Initial fallback sample rate\n\
  -f, --format FORMAT    Initial fallback CamillaDSP sample format (e.g. S32_LE)\n\
  -c, --channels N       Initial fallback capture channel count\n\
  -l, --log-level LEVEL  DEBUG, INFO, WARNING, ERROR, CRITICAL\n\
      --probe            Read snd-aloop controls once and exit\n\
      --ws-check         Connect, query CamillaDSP version, close, exit\n\
      --ws-validate      Adapt YAML and ValidateConfig over websocket\n\
      --adapt-check      Adapt YAML once, write result to stdout, exit\n\
  -h, --help             Show this help\n\
  -V, --version          Show version"
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
            "--probe" => args.mode = Mode::Probe,
            "--ws-check" => args.mode = Mode::WsCheck,
            "--ws-validate" => args.mode = Mode::WsValidate,
            "--adapt-check" => args.mode = Mode::AdaptCheck,
            "--host" => args.host = next_value("--host")?,
            "-p" | "--port" => {
                args.port = next_value("--port")?
                    .parse()
                    .map_err(|_| app_error("--port must be an integer from 1 to 65535"))?;
            }
            "-d" | "--device" => args.device = next_value("--device")?,
            "-a" | "--adapt" => args.adapt = Some(PathBuf::from(next_value("--adapt")?)),
            "-r" | "--rate" => {
                args.initial_rate = Some(
                    next_value("--rate")?
                        .parse()
                        .map_err(|_| app_error("--rate must be a positive integer"))?,
                );
            }
            "-f" | "--format" => args.initial_format = Some(next_value("--format")?),
            "-c" | "--channels" => {
                args.initial_channels = Some(
                    next_value("--channels")?
                        .parse()
                        .map_err(|_| app_error("--channels must be a positive integer"))?,
                );
            }
            "-l" | "--log-level" => {
                args.log_level = LogLevel::parse(&next_value("--log-level")?)?;
            }
            other => return Err(app_error(format!("unknown argument: {other}"))),
        }
    }

    if matches!(args.mode, Mode::Run | Mode::WsValidate | Mode::AdaptCheck)
        && args.adapt.is_none()
    {
        return Err(app_error("this mode requires --adapt PATH"));
    }
    Ok(Some(args))
}
