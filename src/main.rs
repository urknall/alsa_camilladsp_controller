mod adapt;
mod alsa_listener;
mod args;
mod backend;
mod benchmark;
mod camilla_ws;
mod controller;
mod error;
mod logging;
mod wave;

pub mod camilladsp;
pub mod core;
pub mod ipc;

use adapt::{
    adapt_config, get_config_path, get_playback_device, get_state_fragment, make_bypass_config,
    make_statefile,
};
use alsa_listener::AlsaLoopbackListener;
use args::{parse_args, Args, Backend, Mode};
use benchmark::{
    make_benchmark_plan_template, make_benchmark_report, run_benchmark_both_backends,
    validate_benchmark_plan, BenchmarkRunnerConfig,
};
use camilla_ws::{CamillaClient, CamillaWs};
use controller::{new_aloop_controller, run_ioplug};
use error::{app_error, AppResult};
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

        Mode::GetPlaybackDevice => {
            let path = args.config_path.as_deref().expect("validated");
            let device = get_playback_device(path)?;
            println!("{device}");
            Ok(())
        }

        Mode::GetConfigPath => {
            let path = args.config_path.as_deref().expect("validated");
            let config = get_config_path(path)?;
            println!("{config}");
            Ok(())
        }

        Mode::GetStateFragment => {
            let path = args.config_path.as_deref().expect("validated");
            let fragment = get_state_fragment(path)?;
            print!("{fragment}");
            Ok(())
        }

        Mode::MakeBypass => {
            let device = args.playback_device.as_deref().expect("validated");
            let yaml = make_bypass_config(device)?;
            if let Some(output) = args.output.as_deref() {
                std::fs::write(output, &yaml).map_err(|err| {
                    app_error(format!("unable to write {}: {err}", output.display()))
                })?;
            } else {
                print!("{yaml}");
            }
            Ok(())
        }

        Mode::WsGetConfigPath => {
            let mut client = CamillaWs::connect(&args.host, args.port)?;
            let value = client.query("GetConfigFilePath", None)?;
            let path = value
                .and_then(|v| v.as_str().map(str::to_owned))
                .ok_or_else(|| app_error("CamillaDSP returned no config file path"))?;
            client.close();
            println!("{path}");
            Ok(())
        }

        Mode::MakeStatefile => {
            let config_path = args
                .statefile_config_path
                .as_deref()
                .expect("validated: --make-statefile requires --config-path");
            let output = args
                .output
                .as_deref()
                .expect("validated: --make-statefile requires --output");
            let yaml = make_statefile(config_path, args.existing_state.as_deref())?;
            std::fs::write(output, &yaml)
                .map_err(|err| app_error(format!("unable to write {}: {err}", output.display())))?;
            Ok(())
        }

        Mode::MakeBenchmarkPlan => {
            let yaml = make_benchmark_plan_template()?;
            if let Some(output) = args.output.as_deref() {
                std::fs::write(output, &yaml).map_err(|err| {
                    app_error(format!("unable to write {}: {err}", output.display()))
                })?;
            } else {
                print!("{yaml}");
            }
            Ok(())
        }

        Mode::ValidateBenchmarkPlan => {
            let path = args
                .benchmark_path
                .as_deref()
                .ok_or_else(|| app_error("--validate-benchmark-plan requires a path"))?;
            let plan = validate_benchmark_plan(path)?;
            println!(
                "Benchmark plan OK: backends=aloop,ioplug sample_rates={:?} chunksize={} queuelimit={}",
                plan.environment.sample_rates_hz, plan.environment.chunksize, plan.environment.queuelimit
            );
            Ok(())
        }

        Mode::MakeBenchmarkReport => {
            let path = args
                .benchmark_path
                .as_deref()
                .ok_or_else(|| app_error("--make-benchmark-report requires a path"))?;
            let report = make_benchmark_report(path)?;
            if let Some(output) = args.output.as_deref() {
                std::fs::write(output, &report).map_err(|err| {
                    app_error(format!("unable to write {}: {err}", output.display()))
                })?;
            } else {
                print!("{report}");
            }
            Ok(())
        }

        Mode::RunBenchmark => {
            let cfg = BenchmarkRunnerConfig {
                host: args.host.clone(),
                port: args.port,
                aloop_device: args.device.clone(),
            };
            let yaml = run_benchmark_both_backends(&cfg)?;
            if let Some(output) = args.output.as_deref() {
                std::fs::write(output, &yaml).map_err(|err| {
                    app_error(format!("unable to write {}: {err}", output.display()))
                })?;
                eprintln!(
                    "picoredsp-controller --run-benchmark: plan written to {}",
                    output.display()
                );
            } else {
                print!("{yaml}");
            }
            Ok(())
        }

        Mode::Run => match args.backend {
            Backend::Aloop => {
                let (controller, initial) = new_aloop_controller(&args)?;
                controller.run(initial)
            }
            Backend::Ioplug => run_ioplug(&args),
        },
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
