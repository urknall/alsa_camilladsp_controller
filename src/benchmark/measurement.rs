// ─── Per-backend measurement ───────────────────────────────────────────────

use super::collectors::{
    collect_aloop_timings, collect_cdsp_buffer_latency_ms, collect_cdsp_rate,
    collect_context_switches, collect_cpu_percent, collect_pcm_transport_latency_ms,
    collect_rss_kib, find_alsa_card_number, find_controller_pid,
};
use super::report::{BenchmarkEnvironment, BenchmarkMetrics, BenchmarkRun};
use super::runner::BenchmarkRunnerConfig;
use crate::args::Backend;

pub(crate) fn measure_backend(
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
            let software_visible_ms = add_optional(pcm_transport_ms, cdsp_buf_ms);

            BenchmarkRun {
                backend: Backend::Aloop,
                metrics: BenchmarkMetrics {
                    playback_start_latency_ms: start_ms,
                    transition_44_1_to_48_ms: None,
                    transition_48_to_96_ms: None,
                    stop_latency_ms: stop_ms,
                    pcm_transport_latency_ms: pcm_transport_ms,
                    software_visible_latency_ms: software_visible_ms,
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

            let software_visible_ms = add_optional(pcm_transport_ms, cdsp_buf_ms);

            BenchmarkRun {
                backend: Backend::Ioplug,
                metrics: BenchmarkMetrics {
                    playback_start_latency_ms: None,
                    transition_44_1_to_48_ms: None,
                    transition_48_to_96_ms: None,
                    stop_latency_ms: None,
                    pcm_transport_latency_ms: pcm_transport_ms,
                    software_visible_latency_ms: software_visible_ms,
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
}
