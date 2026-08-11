mod adapt;
mod alsa_listener;
mod args;
mod camilla_ws;
mod controller;
mod error;
mod logging;
mod wave;

use adapt::adapt_config;
use alsa_listener::AlsaLoopbackListener;
use args::{parse_args, Args, Mode};
use camilla_ws::CamillaWs;
use controller::Controller;
use error::AppResult;
use serde_json::Value as JsonValue;
use wave::WaveFormat;

fn run_main() -> AppResult<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };

    match args.mode {
        Mode::Probe => {
            let listener = AlsaLoopbackListener::new(&args.device, args.log_level)?;
            let snapshot = listener.read_snapshot()?;
            println!(
                "active={} rate={} format={} channels={}",
                snapshot.active,
                snapshot
                    .wave
                    .sample_rate
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                snapshot.wave.sample_format.as_deref().unwrap_or("unknown"),
                snapshot
                    .wave
                    .channels
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
            );
            Ok(())
        }

        Mode::WsCheck => {
            let mut client = CamillaWs::connect(&args.host, args.port)?;
            let version = client.query("GetVersion", None)?;
            println!(
                "CamillaDSP websocket OK, version={}",
                version
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".to_owned())
            );
            client.close();
            Ok(())
        }

        Mode::WsValidate => {
            let wave = wave_from_args(&args);
            let adapted = adapt_config(args.adapt.as_deref().expect("validated"), &wave)?;
            let mut client = CamillaWs::connect(&args.host, args.port)?;
            let _ = client.query("ValidateConfig", Some(JsonValue::String(adapted)))?;
            println!("CamillaDSP websocket ValidateConfig OK");
            client.close();
            Ok(())
        }

        Mode::AdaptCheck => {
            let wave = wave_from_args(&args);
            let adapted = adapt_config(args.adapt.as_deref().expect("validated"), &wave)?;
            print!("{adapted}");
            Ok(())
        }

        Mode::Run => {
            let (controller, initial) = Controller::new(&args)?;
            controller.run(initial)
        }
    }
}

/// Build a `WaveFormat` from the CLI initial-value flags.
fn wave_from_args(args: &Args) -> WaveFormat {
    WaveFormat {
        sample_rate: args.initial_rate,
        sample_format: args.initial_format.clone(),
        channels: args.initial_channels,
    }
}

fn main() {
    if let Err(err) = run_main() {
        eprintln!("ERROR - {err}");
        std::process::exit(1);
    }
}
