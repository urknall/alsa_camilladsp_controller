use crate::args::Backend;
use crate::camilladsp::websocket::{CamillaClient, CamillaWs};
use crate::error::{app_error, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const BENCHMARK_PLAN_VERSION: u32 = 1;
const REQUIRED_SAMPLE_RATES_HZ: [u32; 4] = [44100, 48000, 96000, 192000];

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkPlan {
    pub version: u32,
    pub environment: BenchmarkEnvironment,
    pub runs: Vec<BenchmarkRun>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkEnvironment {
    pub raspberry_pi: String,
    pub picoreplayer_version: String,
    pub camilladsp_version: String,
    pub dac: String,
    pub dsp_config: String,
    pub track: String,
    pub chunksize: u32,
    pub queuelimit: u32,
    pub sample_rates_hz: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkRun {
    pub backend: Backend,
    pub metrics: BenchmarkMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BenchmarkMetrics {
    pub playback_start_latency_ms: Option<f64>,
    pub transition_44_1_to_48_ms: Option<f64>,
    pub transition_48_to_96_ms: Option<f64>,
    pub stop_latency_ms: Option<f64>,
    pub pcm_transport_latency_ms: Option<f64>,
    pub total_end_to_end_latency_ms: Option<f64>,
    pub cpu_usage_percent: Option<f64>,
    pub context_switches: Option<u64>,
    pub controller_rss_kib: Option<u64>,
    pub plugin_overhead_percent: Option<f64>,
    pub xrun_count: Option<u64>,
    pub stability_24h_passed: Option<bool>,
    pub stability_7d_passed: Option<bool>,
    pub recovery_after_dac_error_passed: Option<bool>,
}

pub fn make_benchmark_plan_template() -> AppResult<String> {
    let plan = BenchmarkPlan {
        version: BENCHMARK_PLAN_VERSION,
        environment: BenchmarkEnvironment {
            raspberry_pi: "Raspberry Pi model".to_owned(),
            picoreplayer_version: "piCorePlayer version".to_owned(),
            camilladsp_version: "CamillaDSP version".to_owned(),
            dac: "DAC model".to_owned(),
            dsp_config: "Config path or fingerprint".to_owned(),
            track: "Reference track / source".to_owned(),
            chunksize: 1024,
            queuelimit: 4,
            sample_rates_hz: REQUIRED_SAMPLE_RATES_HZ.to_vec(),
        },
        runs: vec![
            BenchmarkRun {
                backend: Backend::Aloop,
                metrics: BenchmarkMetrics::default(),
                notes: Some("Stable / recommended reference backend".to_owned()),
            },
            BenchmarkRun {
                backend: Backend::Ioplug,
                metrics: BenchmarkMetrics::default(),
                notes: Some("Experimental direct ioplug backend".to_owned()),
            },
        ],
    };

    serde_yaml_ng::to_string(&plan)
        .map_err(|err| app_error(format!("unable to render benchmark plan template: {err}")))
}

pub fn validate_benchmark_plan(path: &Path) -> AppResult<BenchmarkPlan> {
    let text = fs::read_to_string(path)
        .map_err(|err| app_error(format!("unable to read {}: {err}", path.display())))?;
    let plan: BenchmarkPlan = serde_yaml_ng::from_str(&text)
        .map_err(|err| app_error(format!("unable to parse {}: {err}", path.display())))?;
    validate_plan(&plan)?;
    Ok(plan)
}

pub fn make_benchmark_report(path: &Path) -> AppResult<String> {
    let plan = validate_benchmark_plan(path)?;
    Ok(render_benchmark_report(&plan))
}

fn validate_plan(plan: &BenchmarkPlan) -> AppResult<()> {
    if plan.version != BENCHMARK_PLAN_VERSION {
        return Err(app_error(format!(
            "benchmark plan version must be {}, got {}",
            BENCHMARK_PLAN_VERSION, plan.version
        )));
    }

    ensure_nonempty("environment.raspberry_pi", &plan.environment.raspberry_pi)?;
    ensure_nonempty(
        "environment.picoreplayer_version",
        &plan.environment.picoreplayer_version,
    )?;
    ensure_nonempty(
        "environment.camilladsp_version",
        &plan.environment.camilladsp_version,
    )?;
    ensure_nonempty("environment.dac", &plan.environment.dac)?;
    ensure_nonempty("environment.dsp_config", &plan.environment.dsp_config)?;
    ensure_nonempty("environment.track", &plan.environment.track)?;

    if plan.environment.chunksize == 0 {
        return Err(app_error("environment.chunksize must be greater than zero"));
    }
    if plan.environment.queuelimit == 0 {
        return Err(app_error(
            "environment.queuelimit must be greater than zero",
        ));
    }

    let sample_rates: BTreeSet<u32> = plan.environment.sample_rates_hz.iter().copied().collect();
    let required_rates: BTreeSet<u32> = REQUIRED_SAMPLE_RATES_HZ.into_iter().collect();
    if sample_rates != required_rates {
        return Err(app_error(
            "environment.sample_rates_hz must contain exactly [44100, 48000, 96000, 192000]",
        ));
    }

    if plan.runs.len() != 2 {
        return Err(app_error(
            "benchmark plan must contain exactly two runs: one aloop and one ioplug",
        ));
    }

    let backends: BTreeSet<Backend> = plan.runs.iter().map(|run| run.backend).collect();
    let required_backends: BTreeSet<Backend> =
        [Backend::Aloop, Backend::Ioplug].into_iter().collect();
    if backends != required_backends {
        return Err(app_error(
            "benchmark plan must contain exactly one aloop run and one ioplug run",
        ));
    }

    Ok(())
}

fn render_benchmark_report(plan: &BenchmarkPlan) -> String {
    let aloop = plan.runs.iter().find(|run| run.backend == Backend::Aloop);
    let ioplug = plan.runs.iter().find(|run| run.backend == Backend::Ioplug);
    let mut report = String::new();

    writeln!(&mut report, "# Benchmark report").unwrap();
    writeln!(&mut report).unwrap();
    writeln!(
        &mut report,
        "## Environment\n- Raspberry Pi: {}\n- piCorePlayer: {}\n- CamillaDSP: {}\n- DAC: {}\n- DSP config: {}\n- Track: {}\n- chunksize: {}\n- queuelimit: {}\n- sample rates (Hz): {:?}",
        plan.environment.raspberry_pi,
        plan.environment.picoreplayer_version,
        plan.environment.camilladsp_version,
        plan.environment.dac,
        plan.environment.dsp_config,
        plan.environment.track,
        plan.environment.chunksize,
        plan.environment.queuelimit,
        plan.environment.sample_rates_hz
    )
    .unwrap();
    writeln!(&mut report).unwrap();

    writeln!(&mut report, "## Gate 12 coverage").unwrap();
    writeln!(
        &mut report,
        "| Metric | aloop | ioplug | Coverage |\n| --- | --- | --- | --- |"
    )
    .unwrap();

    let mut complete_metrics = 0usize;
    for spec in metric_specs() {
        let aloop_value = aloop
            .map(|run| (spec.get)(&run.metrics))
            .unwrap_or(MetricValue::Missing);
        let ioplug_value = ioplug
            .map(|run| (spec.get)(&run.metrics))
            .unwrap_or(MetricValue::Missing);
        let coverage = if aloop_value.is_present() && ioplug_value.is_present() {
            complete_metrics += 1;
            "complete"
        } else {
            "missing"
        };
        writeln!(
            &mut report,
            "| {} | {} | {} | {} |",
            spec.label,
            aloop_value.render(),
            ioplug_value.render(),
            coverage
        )
        .unwrap();
    }
    writeln!(
        &mut report,
        "\nGate 12 metrics complete for both backends: {}/{}.",
        complete_metrics,
        metric_specs().len()
    )
    .unwrap();
    writeln!(&mut report).unwrap();

    writeln!(&mut report, "## Backend comparison").unwrap();
    writeln!(
        &mut report,
        "| Metric | aloop | ioplug | Preferred |\n| --- | --- | --- | --- |"
    )
    .unwrap();
    for spec in metric_specs() {
        let aloop_value = aloop
            .map(|run| (spec.get)(&run.metrics))
            .unwrap_or(MetricValue::Missing);
        let ioplug_value = ioplug
            .map(|run| (spec.get)(&run.metrics))
            .unwrap_or(MetricValue::Missing);
        writeln!(
            &mut report,
            "| {} | {} | {} | {} |",
            spec.label,
            aloop_value.render(),
            ioplug_value.render(),
            preferred_backend(spec.kind, aloop_value, ioplug_value)
        )
        .unwrap();
    }
    writeln!(&mut report).unwrap();

    writeln!(&mut report, "## Latency-tuning recommendations").unwrap();
    for recommendation in tuning_recommendations(plan) {
        writeln!(&mut report, "- {recommendation}").unwrap();
    }

    report
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MetricKind {
    LowerIsBetter,
    PassIsBetter,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum MetricValue {
    Float(f64),
    Integer(u64),
    Bool(bool),
    Missing,
}

impl MetricValue {
    fn is_present(self) -> bool {
        !matches!(self, Self::Missing)
    }

    fn render(self) -> String {
        match self {
            Self::Float(value) => format!("{value:.3}"),
            Self::Integer(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Missing => "missing".to_owned(),
        }
    }
}

#[derive(Clone, Copy)]
struct MetricSpec {
    label: &'static str,
    kind: MetricKind,
    get: fn(&BenchmarkMetrics) -> MetricValue,
}

fn metric_specs() -> [MetricSpec; 14] {
    [
        MetricSpec {
            label: "Playback start latency (ms)",
            kind: MetricKind::LowerIsBetter,
            get: playback_start_latency,
        },
        MetricSpec {
            label: "44.1 → 48 kHz transition (ms)",
            kind: MetricKind::LowerIsBetter,
            get: transition_44_1_to_48,
        },
        MetricSpec {
            label: "48 → 96 kHz transition (ms)",
            kind: MetricKind::LowerIsBetter,
            get: transition_48_to_96,
        },
        MetricSpec {
            label: "Stop latency (ms)",
            kind: MetricKind::LowerIsBetter,
            get: stop_latency,
        },
        MetricSpec {
            label: "PCM transport latency (ms)",
            kind: MetricKind::LowerIsBetter,
            get: pcm_transport_latency,
        },
        MetricSpec {
            label: "Total end-to-end latency (ms)",
            kind: MetricKind::LowerIsBetter,
            get: total_end_to_end_latency,
        },
        MetricSpec {
            label: "CPU usage (%)",
            kind: MetricKind::LowerIsBetter,
            get: cpu_usage,
        },
        MetricSpec {
            label: "Context switches",
            kind: MetricKind::LowerIsBetter,
            get: context_switches,
        },
        MetricSpec {
            label: "Controller RSS (KiB)",
            kind: MetricKind::LowerIsBetter,
            get: controller_rss,
        },
        MetricSpec {
            label: "Plugin overhead (%)",
            kind: MetricKind::LowerIsBetter,
            get: plugin_overhead,
        },
        MetricSpec {
            label: "XRUN count",
            kind: MetricKind::LowerIsBetter,
            get: xrun_count,
        },
        MetricSpec {
            label: "24h stability",
            kind: MetricKind::PassIsBetter,
            get: stability_24h,
        },
        MetricSpec {
            label: "7d stability",
            kind: MetricKind::PassIsBetter,
            get: stability_7d,
        },
        MetricSpec {
            label: "Recovery after DAC error",
            kind: MetricKind::PassIsBetter,
            get: recovery_after_dac_error,
        },
    ]
}

fn preferred_backend(kind: MetricKind, aloop: MetricValue, ioplug: MetricValue) -> &'static str {
    match (kind, aloop, ioplug) {
        (_, MetricValue::Missing, _) | (_, _, MetricValue::Missing) => "n/a",
        (MetricKind::LowerIsBetter, MetricValue::Float(a), MetricValue::Float(b)) => {
            compare_numeric(a, b)
        }
        (MetricKind::LowerIsBetter, MetricValue::Integer(a), MetricValue::Integer(b)) => {
            compare_numeric(a as f64, b as f64)
        }
        (MetricKind::PassIsBetter, MetricValue::Bool(a), MetricValue::Bool(b)) => {
            compare_boolean(a, b)
        }
        _ => "n/a",
    }
}

fn compare_numeric(aloop: f64, ioplug: f64) -> &'static str {
    if (aloop - ioplug).abs() <= 0.001 {
        "tie"
    } else if aloop < ioplug {
        "aloop"
    } else {
        "ioplug"
    }
}

fn compare_boolean(aloop: bool, ioplug: bool) -> &'static str {
    match (aloop, ioplug) {
        (true, true) | (false, false) => "tie",
        (true, false) => "aloop",
        (false, true) => "ioplug",
    }
}

fn tuning_recommendations(plan: &BenchmarkPlan) -> Vec<String> {
    let aloop = plan.runs.iter().find(|run| run.backend == Backend::Aloop);
    let ioplug = plan.runs.iter().find(|run| run.backend == Backend::Ioplug);
    let mut out = Vec::new();

    let complete_metrics = metric_specs()
        .iter()
        .filter(|spec| {
            let aloop_value = aloop
                .map(|run| (spec.get)(&run.metrics))
                .unwrap_or(MetricValue::Missing);
            let ioplug_value = ioplug
                .map(|run| (spec.get)(&run.metrics))
                .unwrap_or(MetricValue::Missing);
            aloop_value.is_present() && ioplug_value.is_present()
        })
        .count();
    if complete_metrics < metric_specs().len() {
        out.push(format!(
            "Gate 12 is still incomplete: only {complete_metrics}/{} metrics are recorded for both backends. Fill the missing rows before treating any latency winner as final.",
            metric_specs().len()
        ));
    }

    if aloop
        .and_then(|run| run.metrics.total_end_to_end_latency_ms)
        .is_none()
        || ioplug
            .and_then(|run| run.metrics.total_end_to_end_latency_ms)
            .is_none()
    {
        out.push(
            "Total end-to-end latency is still missing for at least one backend; keep an external measurement in the loop because software-visible buffers alone are not enough.".to_owned(),
        );
    }

    for run in plan.runs.iter() {
        let health = classify_health(&run.metrics);
        match health {
            BackendHealth::NeedsStabilityWork => {
                let mut message = format!(
                    "{} shows XRUNs or failed stability/recovery checks; prioritize correctness before reducing latency. Increase ALSA period/buffer sizes, CamillaDSP chunksize/queuelimit",
                    backend_name(run.backend)
                );
                if run.backend == Backend::Ioplug {
                    message.push_str(", and ioplug ringbuffer depth/pipe size");
                }
                message.push('.');
                out.push(message);
            }
            BackendHealth::ReadyForLatencyExperiments => out.push(format!(
                "{} is stable in the recorded checks. It is a good candidate for stepwise latency experiments: lower chunksize/queuelimit first, then re-measure ALSA period/buffer settings and DAC buffering.",
                backend_name(run.backend)
            )),
            BackendHealth::Incomplete => out.push(format!(
                "{} is missing stability or XRUN data; collect 24h, 7d, recovery-after-DAC-error, and XRUN measurements before tuning for minimum latency.",
                backend_name(run.backend)
            )),
        }
    }

    let (aloop_wins, ioplug_wins) = latency_win_counts(plan);
    match aloop_wins.cmp(&ioplug_wins) {
        std::cmp::Ordering::Greater => out.push(format!(
            "aloop currently leads the software-visible latency comparison by {} to {}. Keep it as the reference point while tuning ioplug.",
            aloop_wins, ioplug_wins
        )),
        std::cmp::Ordering::Less => out.push(format!(
            "ioplug currently leads the software-visible latency comparison by {} to {}. Focus Phase 16 tuning on the best stable ioplug chunksize/queuelimit and backend-specific ringbuffer settings.",
            ioplug_wins, aloop_wins
        )),
        std::cmp::Ordering::Equal => out.push(
            "Neither backend is a clear latency winner from the recorded software-visible metrics yet; keep measuring start, stop, transport, and rate-transition timings for both.".to_owned(),
        ),
    }

    out.push(
        "Per Phase 16, tune in this order: ALSA period size, ALSA buffer size, ioplug ringbuffer depth, pipe size, CamillaDSP chunksize/queuelimit, then DAC period/buffer parameters.".to_owned(),
    );
    out
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackendHealth {
    ReadyForLatencyExperiments,
    NeedsStabilityWork,
    Incomplete,
}

fn classify_health(metrics: &BenchmarkMetrics) -> BackendHealth {
    if matches!(metrics.xrun_count, Some(value) if value > 0)
        || matches!(metrics.stability_24h_passed, Some(false))
        || matches!(metrics.stability_7d_passed, Some(false))
        || matches!(metrics.recovery_after_dac_error_passed, Some(false))
    {
        BackendHealth::NeedsStabilityWork
    } else if metrics.xrun_count.is_some()
        && metrics.stability_24h_passed == Some(true)
        && metrics.stability_7d_passed == Some(true)
        && metrics.recovery_after_dac_error_passed == Some(true)
    {
        BackendHealth::ReadyForLatencyExperiments
    } else {
        BackendHealth::Incomplete
    }
}

fn latency_win_counts(plan: &BenchmarkPlan) -> (usize, usize) {
    let aloop = plan
        .runs
        .iter()
        .find(|run| run.backend == Backend::Aloop)
        .map(|run| &run.metrics);
    let ioplug = plan
        .runs
        .iter()
        .find(|run| run.backend == Backend::Ioplug)
        .map(|run| &run.metrics);
    let mut aloop_wins = 0usize;
    let mut ioplug_wins = 0usize;

    for selector in [
        playback_start_latency as fn(&BenchmarkMetrics) -> MetricValue,
        transition_44_1_to_48,
        transition_48_to_96,
        stop_latency,
        pcm_transport_latency,
        total_end_to_end_latency,
    ] {
        let aloop_value = aloop.map(selector).unwrap_or(MetricValue::Missing);
        let ioplug_value = ioplug.map(selector).unwrap_or(MetricValue::Missing);
        match preferred_backend(MetricKind::LowerIsBetter, aloop_value, ioplug_value) {
            "aloop" => aloop_wins += 1,
            "ioplug" => ioplug_wins += 1,
            _ => {}
        }
    }

    (aloop_wins, ioplug_wins)
}

fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Aloop => "aloop",
        Backend::Ioplug => "ioplug",
    }
}

fn playback_start_latency(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .playback_start_latency_ms
        .map(MetricValue::Float)
        .unwrap_or(MetricValue::Missing)
}

fn transition_44_1_to_48(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .transition_44_1_to_48_ms
        .map(MetricValue::Float)
        .unwrap_or(MetricValue::Missing)
}

fn transition_48_to_96(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .transition_48_to_96_ms
        .map(MetricValue::Float)
        .unwrap_or(MetricValue::Missing)
}

fn stop_latency(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .stop_latency_ms
        .map(MetricValue::Float)
        .unwrap_or(MetricValue::Missing)
}

fn pcm_transport_latency(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .pcm_transport_latency_ms
        .map(MetricValue::Float)
        .unwrap_or(MetricValue::Missing)
}

fn total_end_to_end_latency(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .total_end_to_end_latency_ms
        .map(MetricValue::Float)
        .unwrap_or(MetricValue::Missing)
}

fn cpu_usage(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .cpu_usage_percent
        .map(MetricValue::Float)
        .unwrap_or(MetricValue::Missing)
}

fn context_switches(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .context_switches
        .map(MetricValue::Integer)
        .unwrap_or(MetricValue::Missing)
}

fn controller_rss(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .controller_rss_kib
        .map(MetricValue::Integer)
        .unwrap_or(MetricValue::Missing)
}

fn plugin_overhead(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .plugin_overhead_percent
        .map(MetricValue::Float)
        .unwrap_or(MetricValue::Missing)
}

fn xrun_count(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .xrun_count
        .map(MetricValue::Integer)
        .unwrap_or(MetricValue::Missing)
}

fn stability_24h(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .stability_24h_passed
        .map(MetricValue::Bool)
        .unwrap_or(MetricValue::Missing)
}

fn stability_7d(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .stability_7d_passed
        .map(MetricValue::Bool)
        .unwrap_or(MetricValue::Missing)
}

fn recovery_after_dac_error(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .recovery_after_dac_error_passed
        .map(MetricValue::Bool)
        .unwrap_or(MetricValue::Missing)
}

fn ensure_nonempty(field: &str, value: &str) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(app_error(format!("{field} must not be empty")));
    }
    Ok(())
}

// ─── Benchmark runner ──────────────────────────────────────────────────────

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

// ─── Pure text-parsing helpers (unit-testable) ─────────────────────────────

/// Extract `VmRSS: N kB` from `/proc/<pid>/status`, returning N in KiB.
pub fn parse_rss_kib(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Sum `voluntary_ctxt_switches` + `nonvoluntary_ctxt_switches` from
/// `/proc/<pid>/status`.
pub fn parse_context_switches(status: &str) -> Option<u64> {
    let mut voluntary: Option<u64> = None;
    let mut nonvoluntary: Option<u64> = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("voluntary_ctxt_switches:") {
            voluntary = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
            nonvoluntary = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
    }
    match (voluntary, nonvoluntary) {
        (Some(v), Some(nv)) => Some(v + nv),
        (Some(v), None) => Some(v),
        (None, Some(nv)) => Some(nv),
        (None, None) => None,
    }
}

/// Extract `utime + stime` (jiffies) from `/proc/<pid>/stat` (fields 14 and 15).
///
/// The comm field may contain spaces so we locate the last `)` to skip it.
pub fn parse_cpu_jiffies(stat: &str) -> Option<u64> {
    let after_comm = stat.rfind(')')?;
    let rest = &stat[after_comm + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // Relative to the text after ')':
    //   index 0 = state
    //   index 11 = utime
    //   index 12 = stime
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Count XRUN-related lines in `aplay` stderr/stdout output.
///
/// `aplay` writes lines like `aplay: xrun.c:380: ...` or just `XRUN` on
/// underrun events.
pub fn count_xruns_in_aplay_output(text: &str) -> u64 {
    text.lines()
        .filter(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("xrun") || l.contains("overrun") || l.contains("underrun")
        })
        .count() as u64
}

/// Extract the Raspberry Pi hardware/model string from `/proc/cpuinfo`.
pub fn parse_pi_model(cpuinfo: &str) -> String {
    for line in cpuinfo.lines() {
        for prefix in ["Model\t\t:", "Model\t:", "Hardware\t:", "Hardware:"] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let val = rest.trim().to_owned();
                if !val.is_empty() {
                    return val;
                }
            }
        }
    }
    "unknown".to_owned()
}

/// Trim the first non-empty line from a piCorePlayer version file.
pub fn parse_pcp_version(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

/// Derive the aloop playback PCM device from the HCTL control device name.
///
/// The player (squeezelite / aplay) writes to `hw:<card>,1,0` while the
/// controller reads HCTL events on `hw:<card>,0`.
///
/// Expects an ALSA device string with the `hw:` prefix, e.g. `hw:Loopback,0`
/// or `hw:Loopback,0,0`.  Passing a name without `hw:` (e.g. `"Loopback"`)
/// will produce a string like `"Loopback,1,0"` which is not a valid ALSA PCM
/// device — always include the `hw:` prefix.
pub fn aloop_playback_device(control_device: &str) -> String {
    let card = control_device.split(',').next().unwrap_or(control_device);
    format!("{},1,0", card)
}

/// Parse `rate: N ...` from a `/proc/asound/card*/pcm*/sub*/hw_params` file.
pub fn parse_proc_hwparams_rate(text: &str) -> Option<u32> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("rate:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Parse `period_size: N` from a `/proc/asound/card*/pcm*/sub*/hw_params` file.
pub fn parse_proc_hwparams_period_size(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("period_size:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

// ─── Live system collectors ────────────────────────────────────────────────

/// Scan `/proc/*/cmdline` for a running `picoredsp-controller --run` (daemon)
/// process and return its PID.  Returns `None` if no daemon is found.
pub fn find_controller_pid() -> Option<u32> {
    let proc_dir = std::fs::read_dir("/proc").ok()?;
    let own_pid = std::process::id();

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let pid: u32 = match name_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if pid == own_pid {
            continue;
        }
        let cmdline_path = format!("/proc/{pid}/cmdline");
        let cmdline = match std::fs::read(&cmdline_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        // argv[0] is the first NUL-terminated entry.
        let argv0_end = cmdline
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(cmdline.len());
        let argv0 = std::str::from_utf8(&cmdline[..argv0_end]).unwrap_or("");
        if !argv0.ends_with("picoredsp-controller") {
            continue;
        }
        // Reject our own benchmark invocations.
        let all_args = cmdline
            .split(|&b| b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect::<Vec<_>>()
            .join(" ");
        if !all_args.contains("--run-benchmark") && !all_args.contains("--make-benchmark") {
            return Some(pid);
        }
    }
    None
}

/// Measure the CPU usage percentage for `pid` over `interval_ms` milliseconds
/// by reading `/proc/<pid>/stat` before and after sleeping.
pub fn collect_cpu_percent(pid: u32, interval_ms: u64) -> Option<f64> {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    let hz = hz as f64;

    let stat_path = format!("/proc/{pid}/stat");
    let before = std::fs::read_to_string(&stat_path)
        .ok()
        .and_then(|s| parse_cpu_jiffies(&s))?;
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_millis(interval_ms));
    let after = std::fs::read_to_string(&stat_path)
        .ok()
        .and_then(|s| parse_cpu_jiffies(&s))?;
    let elapsed_s = t0.elapsed().as_secs_f64();

    let delta = after.saturating_sub(before) as f64;
    Some((delta / hz / elapsed_s) * 100.0)
}

/// Read RSS (KiB) for `pid` from `/proc/<pid>/status`.
pub fn collect_rss_kib(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .as_deref()
        .and_then(parse_rss_kib)
}

/// Read total context-switch count for `pid` from `/proc/<pid>/status`.
pub fn collect_context_switches(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .as_deref()
        .and_then(parse_context_switches)
}

/// Find the ALSA card number from a control-device string such as
/// `hw:Loopback,0`.  Tries a numeric card index first, then searches
/// `/proc/asound/cards` for a matching card name.
pub fn find_alsa_card_number(control_device: &str) -> Option<u32> {
    let card_part = control_device.trim_start_matches("hw:").split(',').next()?;
    if let Ok(n) = card_part.parse::<u32>() {
        return Some(n);
    }
    let cards = std::fs::read_to_string("/proc/asound/cards").ok()?;
    let needle = card_part.to_ascii_lowercase();
    for line in cards.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, '[');
        let num_part = parts.next()?.trim();
        let name_part = parts.next()?.split(']').next()?.trim().to_ascii_lowercase();
        if name_part.starts_with(&needle) {
            return num_part.parse().ok();
        }
    }
    None
}

/// Read the PCM transport latency from
/// `/proc/asound/card<N>/pcm*/sub0/hw_params` while an ALSA stream is active.
/// Returns `None` if the file is absent or the stream has not been opened yet.
pub fn collect_pcm_transport_latency_ms(card_num: u32) -> Option<f64> {
    collect_pcm_transport_latency_ms_from(card_num, "/proc/asound")
}

/// Inner implementation parameterised over the `/proc/asound` base path so
/// that unit tests can point it at a temporary directory.
fn collect_pcm_transport_latency_ms_from(card_num: u32, base: &str) -> Option<f64> {
    // snd-aloop creates up to 8 subdevices per PCM stream by default.  The
    // active subdevice index is not necessarily 0 (e.g. Squeezelite may open
    // sub0 while CamillaDSP captures on sub1).  Scan sub0–sub7 for each PCM
    // direction and return the first one that reports an active rate.
    for pcm in ["pcm0p", "pcm0c", "pcm1p", "pcm1c"] {
        for sub in 0u32..8 {
            let path = format!("{base}/card{card_num}/{pcm}/sub{sub}/hw_params");
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let (Some(period), Some(rate)) = (
                    parse_proc_hwparams_period_size(&text),
                    parse_proc_hwparams_rate(&text),
                ) {
                    if rate > 0 {
                        return Some(period as f64 / rate as f64 * 1000.0);
                    }
                }
            }
        }
    }
    None
}

/// Query CamillaDSP over WebSocket for the active pipeline buffer latency in
/// milliseconds (`GetBuffersize / GetSamplerate * 1000`).
///
/// Returns `None` if CamillaDSP is unreachable or not currently processing.
pub fn collect_cdsp_buffer_latency_ms(host: &str, port: u16) -> Option<f64> {
    let mut client = CamillaWs::connect(host, port).ok()?;
    let rate_val = client.query("GetSamplerate", None).ok()??;
    let bufsize_val = client.query("GetBuffersize", None).ok()??;
    let rate = rate_val.as_u64()?;
    let bufsize = bufsize_val.as_u64()?;
    client.close();
    if rate == 0 {
        return None;
    }
    Some(bufsize as f64 / rate as f64 * 1000.0)
}

/// Query CamillaDSP for its active sample rate.  Returns `None` on failure.
fn collect_cdsp_rate(host: &str, port: u16) -> Option<u64> {
    let mut client = CamillaWs::connect(host, port).ok()?;
    let val = client.query("GetSamplerate", None).ok()??;
    let rate = val.as_u64();
    client.close();
    rate
}

/// Query CamillaDSP for its version string.  Returns `"unknown"` on failure.
fn collect_cdsp_version(host: &str, port: u16) -> String {
    CamillaWs::connect(host, port)
        .ok()
        .and_then(|mut c| {
            let v = c.query("GetVersion", None).ok()??;
            let s = v.as_str().map(str::to_owned);
            c.close();
            s
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Spawn a short `aplay` test through the aloop playback device, timing how
/// long it takes for the loopback HCTL to report `active = true` (start
/// latency) and `active = false` after the process is killed (stop latency).
///
/// If another player (e.g. Squeezelite) already has the loopback active when
/// this function is called, `aplay` cannot open the same subdevice.  In that
/// case the function returns start latency as `None` and stop latency as
/// `None` — both are only meaningful when no other player holds the device.
///
/// Returns `(start_latency_ms, stop_latency_ms, xrun_count)`.
/// All are `None` / 0 if ALSA or `aplay` is unavailable.
pub fn collect_aloop_timings(control_device: &str) -> (Option<f64>, Option<f64>, u64) {
    use crate::camilladsp::alsa_capture::AlsaLoopbackListener;
    use crate::core::logging::LogLevel;

    let listener = match AlsaLoopbackListener::new(control_device, LogLevel::Error) {
        Ok(l) => l,
        Err(_) => return (None, None, 0),
    };

    // If a player is already active the loopback write side may be held open
    // (EBUSY).  Detect this before spawning aplay: if active is already true,
    // our timing measurements would be unreliable (start would read near-zero
    // because the snapshot is already active, and stop would never arrive
    // because the other player keeps the device open after we kill aplay).
    let already_active = listener.read_snapshot().map(|s| s.active).unwrap_or(false);
    if already_active {
        return (None, None, 0);
    }

    let playback_dev = aloop_playback_device(control_device);

    // Pass `-v` so that ALSA XRUN events appear in stderr output.
    let mut child = match std::process::Command::new("aplay")
        .args([
            "-D",
            &playback_dev,
            "-r",
            "44100",
            "-c",
            "2",
            "-f",
            "S16_LE",
            "-d",
            "5",
            "-v",
            "/dev/zero",
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (None, None, 0),
    };

    // Poll for active = true (playback start latency).
    let t0 = Instant::now();
    let start_latency_ms = loop {
        if t0.elapsed() > Duration::from_secs(3) {
            break None;
        }
        if listener.read_snapshot().map(|s| s.active).unwrap_or(false) {
            break Some(t0.elapsed().as_secs_f64() * 1000.0);
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // Let it play for 1 second so any xruns can accumulate.
    std::thread::sleep(Duration::from_secs(1));

    // Kill aplay.  Start the stop-latency clock before waiting for the process
    // to exit so that t1 begins as close to the kill signal as possible.
    let _ = child.kill();
    let t1 = Instant::now();
    let xrun_count = match child.wait_with_output() {
        Ok(out) => count_xruns_in_aplay_output(&String::from_utf8_lossy(&out.stderr)),
        Err(_) => 0,
    };

    // Poll for active = false (playback stop latency).
    let stop_latency_ms = loop {
        if t1.elapsed() > Duration::from_secs(3) {
            break None;
        }
        if !listener.read_snapshot().map(|s| s.active).unwrap_or(true) {
            break Some(t1.elapsed().as_secs_f64() * 1000.0);
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    (start_latency_ms, stop_latency_ms, xrun_count)
}

// ─── Environment auto-detection ───────────────────────────────────────────

/// Auto-detect the benchmark environment from system files and a live
/// CamillaDSP WebSocket connection.
pub fn detect_environment(host: &str, port: u16, aloop_device: &str) -> BenchmarkEnvironment {
    let raspberry_pi = std::fs::read_to_string("/proc/cpuinfo")
        .map(|s| parse_pi_model(&s))
        .unwrap_or_else(|_| "unknown".to_owned());

    let picoreplayer_version = ["/usr/local/pcp_version", "/etc/pcp_version"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| parse_pcp_version(&s))
        .unwrap_or_else(|| "unknown".to_owned());

    let camilladsp_version = collect_cdsp_version(host, port);

    let dac = std::fs::read_to_string("/proc/asound/cards")
        .map(|text| detect_dac_from_cards(&text, aloop_device))
        .unwrap_or_else(|_| "unknown".to_owned());

    // Read chunksize from CamillaDSP if available; default to 1024.
    let chunksize = {
        let chunksize_val = CamillaWs::connect(host, port).ok().and_then(|mut c| {
            let v = c.query("GetBuffersize", None).ok()?;
            c.close();
            v.and_then(|j| j.as_u64())
        });
        chunksize_val.map(|n| n as u32).unwrap_or(1024)
    };

    BenchmarkEnvironment {
        raspberry_pi,
        picoreplayer_version,
        camilladsp_version,
        dac,
        dsp_config: "auto-detected (see CamillaDSP active config path)".to_owned(),
        track: "silence via aplay /dev/zero (automated benchmark)".to_owned(),
        chunksize,
        queuelimit: 4,
        sample_rates_hz: REQUIRED_SAMPLE_RATES_HZ.to_vec(),
    }
}

/// Return the first non-loopback ALSA card description from `/proc/asound/cards`.
fn detect_dac_from_cards(cards_text: &str, aloop_device: &str) -> String {
    let loopback_name = aloop_device
        .trim_start_matches("hw:")
        .split(',')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    for line in cards_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Lines look like: " 0 [Loopback       ]: Loopback - Loopback"
        if let Some(bracket_pos) = line.find('[') {
            let after_open = &line[bracket_pos + 1..];
            let name_end = after_open.find(']').unwrap_or(after_open.len());
            let name = after_open[..name_end].trim().to_ascii_lowercase();
            if !name.starts_with(&loopback_name) {
                // Return the description part after the ':'
                if let Some(desc_pos) = line.find(':') {
                    return line[desc_pos + 1..].trim().to_owned();
                }
                return name;
            }
        }
    }
    "unknown".to_owned()
}

// ─── Per-backend measurement ───────────────────────────────────────────────

fn measure_backend(
    backend: Backend,
    cfg: &BenchmarkRunnerConfig,
    env: &BenchmarkEnvironment,
) -> BenchmarkRun {
    // Prefer the running controller daemon's PID for resource metrics; fall
    // back to self if the daemon is not found.
    let measure_pid = find_controller_pid().unwrap_or_else(std::process::id);

    let rss_kib = collect_rss_kib(measure_pid);
    let ctx_switches = collect_context_switches(measure_pid);
    // CPU: 2-second window.
    let cpu_percent = collect_cpu_percent(measure_pid, 2000);

    match backend {
        Backend::Aloop => {
            let (start_ms, stop_ms, xruns) = collect_aloop_timings(&cfg.aloop_device);

            let card_num = find_alsa_card_number(&cfg.aloop_device);
            let pcm_transport_ms = card_num.and_then(collect_pcm_transport_latency_ms);

            let cdsp_buf_ms = collect_cdsp_buffer_latency_ms(&cfg.host, cfg.port);
            let total_e2e_ms = add_optional(pcm_transport_ms, cdsp_buf_ms);

            BenchmarkRun {
                backend: Backend::Aloop,
                metrics: BenchmarkMetrics {
                    playback_start_latency_ms: start_ms,
                    transition_44_1_to_48_ms: None,
                    transition_48_to_96_ms: None,
                    stop_latency_ms: stop_ms,
                    pcm_transport_latency_ms: pcm_transport_ms,
                    total_end_to_end_latency_ms: total_e2e_ms,
                    cpu_usage_percent: cpu_percent,
                    context_switches: ctx_switches,
                    controller_rss_kib: rss_kib,
                    plugin_overhead_percent: None,
                    xrun_count: Some(xruns),
                    stability_24h_passed: None,
                    stability_7d_passed: None,
                    recovery_after_dac_error_passed: None,
                },
                notes: Some(
                    "Automatically measured. \
                     Rate-transition latencies (44.1→48 kHz, 48→96 kHz), plugin overhead, \
                     and long-running stability/recovery tests require manual collection."
                        .to_owned(),
                ),
            }
        }

        Backend::Ioplug => {
            // For the ioplug backend we collect process-level and CamillaDSP WS
            // metrics.  Playback timing (start/stop latency, XRUN count) requires
            // an active ioplug stream driven by the real audio player — it cannot
            // be driven by aplay through the loopback.
            let cdsp_buf_ms = collect_cdsp_buffer_latency_ms(&cfg.host, cfg.port);

            // PCM transport estimate: ioplug passes audio directly to CamillaDSP
            // stdin in chunksize-frame blocks.  One chunksize at the active rate
            // is a reasonable upper bound.
            let pcm_transport_ms = collect_cdsp_rate(&cfg.host, cfg.port).and_then(|rate| {
                if rate == 0 {
                    return None;
                }
                Some(env.chunksize as f64 / rate as f64 * 1000.0)
            });

            let total_e2e_ms = add_optional(pcm_transport_ms, cdsp_buf_ms);

            BenchmarkRun {
                backend: Backend::Ioplug,
                metrics: BenchmarkMetrics {
                    playback_start_latency_ms: None,
                    transition_44_1_to_48_ms: None,
                    transition_48_to_96_ms: None,
                    stop_latency_ms: None,
                    pcm_transport_latency_ms: pcm_transport_ms,
                    total_end_to_end_latency_ms: total_e2e_ms,
                    cpu_usage_percent: cpu_percent,
                    context_switches: ctx_switches,
                    controller_rss_kib: rss_kib,
                    plugin_overhead_percent: None,
                    xrun_count: None,
                    stability_24h_passed: None,
                    stability_7d_passed: None,
                    recovery_after_dac_error_passed: None,
                },
                notes: Some(
                    "Automatically measured (process-level and CamillaDSP WS metrics only). \
                     Playback timing (start/stop latency, XRUN count) requires an active \
                     ioplug stream driven by the real audio player — run the player through \
                     the ioplug device and re-measure with a running CamillaDSP instance. \
                     Rate-transition latencies, plugin overhead, and stability/recovery tests \
                     require manual collection."
                        .to_owned(),
                ),
            }
        }
    }
}

/// Return `Some(a + b)` if at least one is `Some`, treating `None` as 0 when
/// the other value is `Some`.  Returns `None` only if both are `None`.
fn add_optional(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_template() -> BenchmarkPlan {
        let yaml = make_benchmark_plan_template().expect("template");
        serde_yaml_ng::from_str(&yaml).expect("valid template yaml")
    }

    #[test]
    fn generated_template_is_valid_and_contains_both_backends() {
        let plan = parse_template();
        validate_plan(&plan).expect("template should validate");
        assert_eq!(plan.version, BENCHMARK_PLAN_VERSION);
        assert_eq!(plan.environment.sample_rates_hz, REQUIRED_SAMPLE_RATES_HZ);
        assert_eq!(plan.runs.len(), 2);
        assert_eq!(plan.runs[0].backend, Backend::Aloop);
        assert_eq!(plan.runs[1].backend, Backend::Ioplug);
    }

    #[test]
    fn validation_rejects_missing_required_sample_rate() {
        let mut plan = parse_template();
        plan.environment.sample_rates_hz = vec![44100, 48000, 96000];
        let err = validate_plan(&plan).expect_err("missing rate must fail");
        assert!(
            err.to_string()
                .contains("environment.sample_rates_hz must contain exactly"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validation_rejects_duplicate_backend_runs() {
        let mut plan = parse_template();
        plan.runs[1].backend = Backend::Aloop;
        let err = validate_plan(&plan).expect_err("duplicate backend must fail");
        assert!(
            err.to_string()
                .contains("benchmark plan must contain exactly one aloop run and one ioplug run"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validation_rejects_blank_environment_fields() {
        let mut plan = parse_template();
        plan.environment.track = "   ".to_owned();
        let err = validate_plan(&plan).expect_err("blank field must fail");
        assert!(
            err.to_string()
                .contains("environment.track must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn benchmark_report_marks_template_metrics_as_missing() {
        let plan = parse_template();
        let report = render_benchmark_report(&plan);
        assert!(report.contains("# Benchmark report"));
        assert!(report.contains("Gate 12 metrics complete for both backends: 0/14."));
        assert!(report.contains("Total end-to-end latency is still missing"));
    }

    #[test]
    fn benchmark_report_picks_latency_winner_when_metrics_are_present() {
        let mut plan = parse_template();
        let aloop = plan
            .runs
            .iter_mut()
            .find(|run| run.backend == Backend::Aloop)
            .expect("aloop run");
        aloop.metrics.playback_start_latency_ms = Some(12.0);
        aloop.metrics.transition_44_1_to_48_ms = Some(18.0);
        aloop.metrics.transition_48_to_96_ms = Some(19.0);
        aloop.metrics.stop_latency_ms = Some(11.0);
        aloop.metrics.pcm_transport_latency_ms = Some(9.0);
        aloop.metrics.total_end_to_end_latency_ms = Some(30.0);
        aloop.metrics.cpu_usage_percent = Some(7.0);
        aloop.metrics.context_switches = Some(100);
        aloop.metrics.controller_rss_kib = Some(2048);
        aloop.metrics.plugin_overhead_percent = Some(0.0);
        aloop.metrics.xrun_count = Some(0);
        aloop.metrics.stability_24h_passed = Some(true);
        aloop.metrics.stability_7d_passed = Some(true);
        aloop.metrics.recovery_after_dac_error_passed = Some(true);

        let ioplug = plan
            .runs
            .iter_mut()
            .find(|run| run.backend == Backend::Ioplug)
            .expect("ioplug run");
        ioplug.metrics.playback_start_latency_ms = Some(10.0);
        ioplug.metrics.transition_44_1_to_48_ms = Some(15.0);
        ioplug.metrics.transition_48_to_96_ms = Some(16.0);
        ioplug.metrics.stop_latency_ms = Some(9.0);
        ioplug.metrics.pcm_transport_latency_ms = Some(5.0);
        ioplug.metrics.total_end_to_end_latency_ms = Some(24.0);
        ioplug.metrics.cpu_usage_percent = Some(6.0);
        ioplug.metrics.context_switches = Some(80);
        ioplug.metrics.controller_rss_kib = Some(1800);
        ioplug.metrics.plugin_overhead_percent = Some(1.2);
        ioplug.metrics.xrun_count = Some(0);
        ioplug.metrics.stability_24h_passed = Some(true);
        ioplug.metrics.stability_7d_passed = Some(true);
        ioplug.metrics.recovery_after_dac_error_passed = Some(true);

        let report = render_benchmark_report(&plan);
        assert!(report.contains("Gate 12 metrics complete for both backends: 14/14."));
        assert!(report.contains("| Playback start latency (ms) | 12.000 | 10.000 | ioplug |"));
        assert!(report.contains("ioplug currently leads the software-visible latency comparison"));
        assert!(report.contains("ioplug is stable in the recorded checks."));
    }

    // ── Collector unit tests ─────────────────────────────────────────────

    #[test]
    fn parse_rss_kib_extracts_vmrss_line() {
        let status = "Name:\tpicoredsp-controller\nVmRSS:\t 2048 kB\nVmPeak:\t 3000 kB\n";
        assert_eq!(parse_rss_kib(status), Some(2048));
    }

    #[test]
    fn parse_rss_kib_returns_none_when_field_absent() {
        let status = "Name:\tfoo\nVmPeak:\t 3000 kB\n";
        assert_eq!(parse_rss_kib(status), None);
    }

    #[test]
    fn parse_context_switches_sums_voluntary_and_nonvoluntary() {
        let status = "voluntary_ctxt_switches:\t100\nnonvoluntary_ctxt_switches:\t25\n";
        assert_eq!(parse_context_switches(status), Some(125));
    }

    #[test]
    fn parse_context_switches_handles_missing_nonvoluntary() {
        let status = "voluntary_ctxt_switches:\t42\n";
        assert_eq!(parse_context_switches(status), Some(42));
    }

    #[test]
    fn parse_context_switches_returns_none_when_both_absent() {
        let status = "Name:\tfoo\n";
        assert_eq!(parse_context_switches(status), None);
    }

    #[test]
    fn parse_cpu_jiffies_extracts_utime_stime() {
        // Minimal /proc/<pid>/stat with a comm that does not contain spaces.
        let stat = "123 (picoredsp) S 1 123 123 0 -1 4194560 0 0 0 0 12 5 0 0 20 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        // utime = field 14 (0-based after ')') = index 11 → 12
        // stime = field 15 → index 12 → 5
        assert_eq!(parse_cpu_jiffies(stat), Some(17));
    }

    #[test]
    fn parse_cpu_jiffies_handles_comm_with_spaces() {
        // comm = "(my prog)" — last ')' is after the comm
        let stat = "42 (my prog) S 1 42 42 0 -1 0 0 0 0 0 8 3 0 0 20 0 1 0 0 0 0";
        assert_eq!(parse_cpu_jiffies(stat), Some(11));
    }

    #[test]
    fn count_xruns_in_aplay_output_counts_xrun_lines() {
        let output = "Playing raw data '/dev/zero' : Signed 16 bit ...\n\
                      aplay: xrun.c:380: ...\n\
                      aplay: xrun.c:380: ...\n\
                      Unrelated line\n";
        assert_eq!(count_xruns_in_aplay_output(output), 2);
    }

    #[test]
    fn count_xruns_in_aplay_output_counts_overrun_and_underrun() {
        let output = "overrun!!!\nunderrun!!!\nnormal line\n";
        assert_eq!(count_xruns_in_aplay_output(output), 2);
    }

    #[test]
    fn count_xruns_in_aplay_output_zero_when_clean() {
        let output = "Playing raw data...\nDone.\n";
        assert_eq!(count_xruns_in_aplay_output(output), 0);
    }

    #[test]
    fn parse_pi_model_extracts_model_field() {
        let cpuinfo = "processor\t: 0\nModel\t\t: Raspberry Pi 4 Model B Rev 1.4\nSerial\t\t: 00000000deadbeef\n";
        assert_eq!(parse_pi_model(cpuinfo), "Raspberry Pi 4 Model B Rev 1.4");
    }

    #[test]
    fn parse_pi_model_falls_back_to_hardware() {
        let cpuinfo = "processor\t: 0\nHardware\t: BCM2711\nRevision\t: c03114\n";
        assert_eq!(parse_pi_model(cpuinfo), "BCM2711");
    }

    #[test]
    fn parse_pi_model_returns_unknown_when_absent() {
        let cpuinfo = "processor\t: 0\n";
        assert_eq!(parse_pi_model(cpuinfo), "unknown");
    }

    #[test]
    fn parse_pcp_version_trims_first_nonempty_line() {
        let text = "\n  9.2.0  \nsome other content\n";
        assert_eq!(parse_pcp_version(text), "9.2.0");
    }

    #[test]
    fn parse_pcp_version_returns_unknown_for_empty_input() {
        assert_eq!(parse_pcp_version(""), "unknown");
    }

    #[test]
    fn aloop_playback_device_derives_playback_side() {
        assert_eq!(aloop_playback_device("hw:Loopback,0"), "hw:Loopback,1,0");
        assert_eq!(aloop_playback_device("hw:Loopback,0,0"), "hw:Loopback,1,0");
        assert_eq!(aloop_playback_device("hw:1,0"), "hw:1,1,0");
    }

    #[test]
    fn parse_proc_hwparams_rate_and_period_size() {
        let hw_params = "access: MMAP_INTERLEAVED\n\
                         format: S16_LE\n\
                         subformat: STD\n\
                         channels: 2\n\
                         rate: 44100 (44100/1)\n\
                         period_size: 1024\n\
                         buffer_size: 4096\n";
        assert_eq!(parse_proc_hwparams_rate(hw_params), Some(44100));
        assert_eq!(parse_proc_hwparams_period_size(hw_params), Some(1024));
    }

    #[test]
    fn parse_proc_hwparams_returns_none_when_fields_absent() {
        let hw_params = "state: PREPARED\n";
        assert_eq!(parse_proc_hwparams_rate(hw_params), None);
        assert_eq!(parse_proc_hwparams_period_size(hw_params), None);
    }

    #[test]
    fn find_alsa_card_number_parses_numeric_card_index() {
        // "hw:1,0" → card 1 (no /proc lookup needed)
        assert_eq!(find_alsa_card_number("hw:1,0"), Some(1));
        assert_eq!(find_alsa_card_number("hw:0,0"), Some(0));
    }

    #[test]
    fn detect_dac_from_cards_skips_loopback_returns_first_other() {
        let cards = " 0 [Loopback       ]: Loopback - Loopback\n \
                     1 [DAC            ]: USB-Audio - My DAC\n";
        let dac = detect_dac_from_cards(cards, "hw:Loopback,0");
        assert!(
            dac.contains("USB-Audio") || dac.contains("My DAC"),
            "got: {dac}"
        );
    }

    #[test]
    fn detect_dac_from_cards_returns_unknown_when_only_loopback_present() {
        let cards = " 0 [Loopback       ]: Loopback - Loopback\n";
        let dac = detect_dac_from_cards(cards, "hw:Loopback,0");
        assert_eq!(dac, "unknown");
    }

    #[test]
    fn add_optional_sums_both_some() {
        assert_eq!(add_optional(Some(1.0), Some(2.0)), Some(3.0));
    }

    #[test]
    fn add_optional_returns_first_when_second_none() {
        assert_eq!(add_optional(Some(5.0), None), Some(5.0));
    }

    #[test]
    fn add_optional_returns_second_when_first_none() {
        assert_eq!(add_optional(None, Some(3.0)), Some(3.0));
    }

    #[test]
    fn add_optional_returns_none_when_both_none() {
        assert_eq!(add_optional(None, None), None);
    }

    #[test]
    fn collect_pcm_transport_scans_multiple_subdevices() {
        // Build a /proc/asound-like tree under /tmp where only sub2 is active.
        // Confirm collect_pcm_transport_latency_ms_from returns Some (the loop
        // reaches sub2) rather than None (which would happen with the old
        // sub0-only code).
        let base = format!("/tmp/pcm_transport_test_{}", std::process::id());
        let card_dir = format!("{base}/card5/pcm0p");
        for sub in 0u32..4 {
            let sub_dir = format!("{card_dir}/sub{sub}");
            std::fs::create_dir_all(&sub_dir).unwrap();
            let content = if sub == 2 {
                "access: MMAP_INTERLEAVED\n\
                 format: S16_LE\n\
                 channels: 2\n\
                 rate: 48000 (48000/1)\n\
                 period_size: 512\n\
                 buffer_size: 4096\n"
            } else {
                "closed\n"
            };
            std::fs::write(format!("{sub_dir}/hw_params"), content).unwrap();
        }

        let result = collect_pcm_transport_latency_ms_from(5, &base);
        // Clean up before asserting so temp files are always removed.
        let _ = std::fs::remove_dir_all(&base);

        // 512 / 48000 * 1000 ≈ 10.666 ms
        let ms = result.expect("should have found rate on sub2");
        assert!((ms - 10.666).abs() < 0.01, "got {ms}");
    }

    #[test]
    fn count_xruns_in_aplay_output_detects_verbose_xrun_lines() {
        // aplay -v emits lines like "aplay: xrun.c:380: read/write error, state = RUNNING"
        let output =
            "Playing raw data '/dev/zero' : Signed 16 bit Little Endian, Rate 44100 Hz, Stereo\n\
                      aplay: xrun.c:380: read/write error, state = RUNNING\n\
                      aplay: xrun.c:380: read/write error, state = RUNNING\n\
                      Aborted by signal Kill...\n";
        assert_eq!(count_xruns_in_aplay_output(output), 2);
    }
}
