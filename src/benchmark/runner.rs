// ─── Benchmark runner ──────────────────────────────────────────────────────

use super::collectors::detect_environment;
use super::measurement::measure_backend;
use super::report::{BenchmarkPlan, BENCHMARK_PLAN_VERSION};
use crate::args::Backend;
use crate::core::errors::{app_error, AppResult};

/// Configuration supplied to the automatic benchmark runner.
pub struct BenchmarkRunnerConfig {
    /// CamillaDSP WebSocket host (default: 127.0.0.1).
    pub host: String,
    /// CamillaDSP WebSocket port (default: 1234).
    pub port: u16,
    /// ALSA HCTL control device for the aloop backend (e.g. `hw:Loopback,0`).
    pub aloop_device: String,
}

/// Run automatic measurements for **both** backends and produce a fully
/// populated `BenchmarkPlan` YAML.
///
/// Metrics that require manual collection (rate-transition latencies, long
/// soak stability tests, hardware fault injection) are left as `null` in the
/// output; the `notes` field on each run explains what still needs to be
/// collected by hand.
pub fn run_benchmark_both_backends(cfg: &BenchmarkRunnerConfig) -> AppResult<String> {
    eprintln!("picoredsp-controller --run-benchmark: detecting environment...");
    let environment = detect_environment(&cfg.host, cfg.port, &cfg.aloop_device);

    eprintln!("picoredsp-controller --run-benchmark: measuring aloop backend...");
    let aloop_run = measure_backend(Backend::Aloop, cfg, &environment);

    eprintln!("picoredsp-controller --run-benchmark: measuring ioplug backend...");
    let ioplug_run = measure_backend(Backend::Ioplug, cfg, &environment);

    let plan = BenchmarkPlan {
        version: BENCHMARK_PLAN_VERSION,
        environment,
        runs: vec![aloop_run, ioplug_run],
    };

    serde_yaml_ng::to_string(&plan)
        .map_err(|err| app_error(format!("unable to serialize benchmark plan: {err}")))
}
