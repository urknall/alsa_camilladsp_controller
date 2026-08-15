use crate::args::Backend;
use crate::core::errors::{app_error, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

pub(crate) const BENCHMARK_PLAN_VERSION: u32 = 1;
pub(crate) const REQUIRED_SAMPLE_RATES_HZ: [u32; 4] = [44100, 48000, 96000, 192000];

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
    pub software_visible_latency_ms: Option<f64>,
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
            label: "Software-visible latency (ms)",
            kind: MetricKind::LowerIsBetter,
            get: software_visible_latency,
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
        .and_then(|run| run.metrics.software_visible_latency_ms)
        .is_none()
        || ioplug
            .and_then(|run| run.metrics.software_visible_latency_ms)
            .is_none()
    {
        out.push(
            "Software-visible latency is still missing for at least one backend; keep an external measurement in the loop because software-visible buffers alone are not enough.".to_owned(),
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
        software_visible_latency,
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

fn software_visible_latency(metrics: &BenchmarkMetrics) -> MetricValue {
    metrics
        .software_visible_latency_ms
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
        assert!(report.contains("Software-visible latency is still missing"));
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
        aloop.metrics.software_visible_latency_ms = Some(30.0);
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
        ioplug.metrics.software_visible_latency_ms = Some(24.0);
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
}
