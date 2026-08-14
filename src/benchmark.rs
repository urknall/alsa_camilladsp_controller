use crate::args::Backend;
use crate::error::{app_error, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

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
}
