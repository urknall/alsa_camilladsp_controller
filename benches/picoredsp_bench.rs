/*!
 * picoredsp-controller — Rust benchmark runner
 *
 * Compiled by `cargo bench` (harness = false; no external crate required).
 * All benchmarks use `std::time::Instant` for measurement and report
 * statistics (n, min, p50, p95, p99, max, mean, stddev) to stdout.
 *
 * Benchmarks
 * ----------
 * benchmark_plan_template_serialize
 *     Serialise a two-run BenchmarkPlan to YAML with serde_yaml_ng.
 *     This is what `make_benchmark_plan_template()` does internally.
 *
 * benchmark_plan_yaml_deserialize
 *     Parse the YAML template back into a BenchmarkPlan struct.
 *     This is what `validate_benchmark_plan()` does before validating.
 *
 * benchmark_plan_validate
 *     Run the full field-validation pass on an in-memory plan (no file I/O).
 *
 * bypass_config_serialize
 *     Serialise a minimal CamillaDSP bypass pipeline config to YAML.
 *     This mirrors `make_bypass_config()` in adaptation.rs.
 */

// Minimal type mirror — matches the production BenchmarkPlan field-for-field
// so the serde_yaml_ng code path is identical.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

// ─── Mirror types ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BenchmarkEnvironment {
    raspberry_pi: String,
    picoreplayer_version: String,
    camilladsp_version: String,
    dac: String,
    dsp_config: String,
    track: String,
    chunksize: u32,
    queuelimit: u32,
    sample_rates_hz: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
enum Backend {
    Aloop,
    Ioplug,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct BenchmarkMetrics {
    playback_start_latency_ms: Option<f64>,
    transition_44_1_to_48_ms: Option<f64>,
    transition_48_to_96_ms: Option<f64>,
    stop_latency_ms: Option<f64>,
    pcm_transport_latency_ms: Option<f64>,
    total_end_to_end_latency_ms: Option<f64>,
    cpu_usage_percent: Option<f64>,
    context_switches: Option<u64>,
    controller_rss_kib: Option<u64>,
    plugin_overhead_percent: Option<f64>,
    xrun_count: Option<u64>,
    stability_24h_passed: Option<bool>,
    stability_7d_passed: Option<bool>,
    recovery_after_dac_error_passed: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BenchmarkRun {
    backend: Backend,
    metrics: BenchmarkMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BenchmarkPlan {
    version: u32,
    environment: BenchmarkEnvironment,
    runs: Vec<BenchmarkRun>,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn make_plan() -> BenchmarkPlan {
    BenchmarkPlan {
        version: 1,
        environment: BenchmarkEnvironment {
            raspberry_pi: "Raspberry Pi 4 Model B".to_owned(),
            picoreplayer_version: "9.1.0".to_owned(),
            camilladsp_version: "2.0.3".to_owned(),
            dac: "Allo Boss2".to_owned(),
            dsp_config: "/etc/camilladsp/default.yml".to_owned(),
            track: "Pink noise 192 kHz / 24-bit reference".to_owned(),
            chunksize: 1024,
            queuelimit: 4,
            sample_rates_hz: vec![44100, 48000, 96000, 192000],
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
    }
}

fn validate_plan(plan: &BenchmarkPlan) -> bool {
    if plan.version != 1 {
        return false;
    }
    let required: BTreeSet<u32> = [44100u32, 48000, 96000, 192000].iter().copied().collect();
    let actual: BTreeSet<u32> = plan.environment.sample_rates_hz.iter().copied().collect();
    if required != actual {
        return false;
    }
    if plan.runs.len() != 2 {
        return false;
    }
    let backends: BTreeSet<Backend> = plan.runs.iter().map(|r| r.backend).collect();
    let req_be: BTreeSet<Backend> = [Backend::Aloop, Backend::Ioplug].iter().copied().collect();
    backends == req_be
}

// ─── Statistics ──────────────────────────────────────────────────────────────

fn bench_stats(name: &str, samples: &mut [Duration]) {
    let n = samples.len();
    if n == 0 {
        println!("  {name:<52}  (no samples)");
        return;
    }
    samples.sort_unstable();

    let ns: Vec<f64> = samples.iter().map(|d| d.as_nanos() as f64).collect();
    let sum: f64 = ns.iter().sum();
    let mean = sum / n as f64;
    let var = ns.iter().map(|&x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1).max(1) as f64;
    let stddev = var.sqrt();

    let pct = |p: usize| ns[(n * p / 100).min(n - 1)];

    println!(
        "  {name:<52}  n={n:<6}  min={:>7.0} ns  p50={:>7.0} ns  p95={:>7.0} ns\
         \n  {blank:<52}               p99={:>7.0} ns  max={:>7.0} ns  mean={:>7.0} ns  stddev={:>6.0} ns",
        pct(0), pct(50), pct(95),
        pct(99), pct(99).max(ns[n - 1]), mean, stddev,
        blank = "",
    );
}

fn bench_section(title: &str) {
    println!("\n{title}");
    println!("{}", "-".repeat(title.len()));
}

// ─── Benchmark runners ───────────────────────────────────────────────────────

const ITERS: usize = 5000;
const WARMUP: usize = 200;

fn bench_serialize() {
    let plan = make_plan();
    // warm-up
    for _ in 0..WARMUP {
        let _ = serde_yaml_ng::to_string(&plan).unwrap();
    }
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let yaml = serde_yaml_ng::to_string(&plan).unwrap();
        samples.push(t0.elapsed());
        // prevent DCE
        assert!(!yaml.is_empty());
    }
    bench_stats("benchmark_plan_template_serialize", &mut samples);
}

fn bench_deserialize() {
    let plan = make_plan();
    let yaml = serde_yaml_ng::to_string(&plan).unwrap();
    // warm-up
    for _ in 0..WARMUP {
        let _: BenchmarkPlan = serde_yaml_ng::from_str(&yaml).unwrap();
    }
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let p: BenchmarkPlan = serde_yaml_ng::from_str(&yaml).unwrap();
        samples.push(t0.elapsed());
        // prevent DCE
        assert!(p.version == 1);
    }
    bench_stats("benchmark_plan_yaml_deserialize", &mut samples);
}

fn bench_validate() {
    let plan = make_plan();
    // warm-up
    for _ in 0..WARMUP {
        assert!(validate_plan(&plan));
    }
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let ok = validate_plan(&plan);
        samples.push(t0.elapsed());
        assert!(ok);
    }
    bench_stats("benchmark_plan_validate", &mut samples);
}

fn bench_roundtrip() {
    // warm-up
    let plan = make_plan();
    for _ in 0..WARMUP {
        let yaml = serde_yaml_ng::to_string(&plan).unwrap();
        let _: BenchmarkPlan = serde_yaml_ng::from_str(&yaml).unwrap();
    }
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let yaml = serde_yaml_ng::to_string(&plan).unwrap();
        let p: BenchmarkPlan = serde_yaml_ng::from_str(&yaml).unwrap();
        samples.push(t0.elapsed());
        assert!(p.version == 1);
    }
    bench_stats(
        "benchmark_plan_yaml_roundtrip (serialize + deserialize)",
        &mut samples,
    );
}

// ─── main ────────────────────────────────────────────────────────────────────

fn main() {
    println!("picoredsp_bench (Rust)");
    println!("======================");
    println!("iters={ITERS}  warmup={WARMUP}");

    bench_section("BenchmarkPlan serialization (serde_yaml_ng)");
    bench_serialize();
    bench_deserialize();
    bench_roundtrip();

    bench_section("BenchmarkPlan validation");
    bench_validate();

    println!("\npicoredsp_bench done");
}
