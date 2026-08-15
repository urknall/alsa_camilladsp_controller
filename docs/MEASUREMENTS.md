# Running automatic measurements and generating a benchmark report

This document explains how to use the `--run-benchmark`, `--make-benchmark-plan`,
`--validate-benchmark-plan`, and `--make-benchmark-report` commands to collect
metrics for both backends and turn them into a formatted report.

## Overview

The measurement workflow has four stages:

1. **Prepare** — generate a canonical benchmark plan template.
2. **Collect** — run the automatic measurement pass (`--run-benchmark`) and, for
   metrics that cannot be measured automatically, fill in the remaining fields by
   hand.
3. **Validate** — confirm the completed plan conforms to the schema.
4. **Report** — render a Markdown report with Gate 12 coverage, backend
   comparisons, and latency-tuning hints.

---

## Prerequisites

| Requirement | Why it is needed |
|---|---|
| `picoredsp-controller` binary on `$PATH` | All CLI commands below use it |
| `snd-aloop` kernel module loaded | `--run-benchmark` uses the loopback ALSA device to drive the aloop backend measurement |
| CamillaDSP running and reachable | Auto-detection queries `GetVersion`, `GetBuffersize`, and `GetConfigFilePath` via WebSocket |
| Loopback **idle** (no other player active) | aloop start/stop latency and XRUN counts require the loopback write side to be free; if Squeezelite or another player is already holding it, those metrics are skipped automatically |

If you have not yet run the installer, download the binary first:

```sh
ARCH=$(uname -m); case "$ARCH" in armv7l) ARCH=armv7 ;; esac
wget -O picoredsp-controller \
  "https://github.com/urknall/alsa_camilladsp_controller/releases/download/installer-latest/picoredsp-controller-${ARCH}"
chmod +x picoredsp-controller
sudo mv picoredsp-controller /usr/local/bin/
```

---

## Stage 1 — Generate the template (optional)

If you want to inspect or hand-edit the YAML before running measurements,
generate an empty template first:

```sh
picoredsp-controller --make-benchmark-plan --output /tmp/benchmark-plan.yml
```

The template contains:

- one `environment` block with placeholder strings for the fixed test setup
- exactly two runs (`aloop`, `ioplug`)
- the required sample-rate set: 44100, 48000, 96000, 192000
- `null` placeholders for all roadmap metrics

You can skip this step and let `--run-benchmark` produce a populated plan
directly.

---

## Stage 2 — Run the automatic measurement pass

```sh
picoredsp-controller --run-benchmark \
  --host 127.0.0.1 \
  --port 1234 \
  --device hw:Loopback,0 \
  --output /tmp/benchmark-plan.yml
```

The flags are optional and default to the values shown above.

### What `--run-benchmark` detects automatically

| Metric | Source |
|---|---|
| `raspberry_pi` | `/proc/cpuinfo` — `Model` or `Hardware` field |
| `picoreplayer_version` | `/usr/local/pcp_version` or `/etc/pcp_version` |
| `camilladsp_version` | CamillaDSP WebSocket `GetVersion` reply |
| `dac` | `/proc/asound/cards` — first non-loopback card description |
| `chunksize` | CamillaDSP WebSocket `GetBuffersize` reply (falls back to 1024) |
| `queuelimit` | Fixed to 4 (roadmap default) |
| `sample_rates_hz` | Fixed required set: 44100, 48000, 96000, 192000 |
| `controller_rss_kib` | `/proc/<pid>/status` — `VmRSS` |
| `context_switches` | `/proc/<pid>/status` — voluntary + nonvoluntary |
| `cpu_usage_percent` | Two-second sampling window via `/proc/<pid>/stat` |
| `playback_start_latency_ms` *(aloop only)* | `aplay` timing via the loopback device |
| `stop_latency_ms` *(aloop only)* | Elapsed time from `aplay` termination |
| `xrun_count` *(aloop only)* | XRUN lines in `aplay` output |
| `pcm_transport_latency_ms` | `/proc/asound/card*/pcm*/sub*/hw_params` rate, combined with CamillaDSP buffer depth |
| `software_visible_latency_ms` | Sum of PCM transport and CamillaDSP buffer latency |

The PID used for resource metrics is the running `picoredsp-controller` daemon
if one is found; otherwise the measurement process itself is used.

### What still requires manual collection

The following fields are left as `null` in the auto-generated YAML and must be
filled in by hand after real-hardware testing:

| Field | How to collect |
|---|---|
| `transition_44_1_to_48_ms` | Play 44.1 kHz audio, switch to 48 kHz, measure wall-clock elapsed until CamillaDSP reports the new config active |
| `transition_48_to_96_ms` | Same as above between 48 kHz and 96 kHz |
| `plugin_overhead_percent` | CPU delta between baseline (no ioplug) and ioplug-routed playback |
| `stability_24h_passed` | Run a 24-hour soak test; record `true` / `false` |
| `stability_7d_passed` | Run a 7-day soak test; record `true` / `false` |
| `recovery_after_dac_error_passed` | Unplug/replug the DAC during playback and verify recovery; record `true` / `false` |

> **Note for the ioplug backend:** `playback_start_latency_ms`, `stop_latency_ms`,
> and `xrun_count` also require manual collection for ioplug, because those timings
> need an active ioplug stream driven by the real audio player (Squeezelite, AirPlay,
> etc.) — they cannot be driven by `aplay` through the loopback.

After filling in the remaining fields, re-run `--validate-benchmark-plan` before
generating the report.

---

## Stage 3 — Validate the completed plan

```sh
picoredsp-controller --validate-benchmark-plan /tmp/benchmark-plan.yml
```

Validation fails unless:

- `version` matches the current benchmark schema version (1)
- all `environment` strings are non-empty
- `chunksize` and `queuelimit` are greater than zero
- `sample_rates_hz` is exactly `[44100, 48000, 96000, 192000]`
- the run list contains exactly one `aloop` run and one `ioplug` run

A successful validation prints a confirmation message and exits 0.

---

## Stage 4 — Generate the Markdown report

```sh
picoredsp-controller --make-benchmark-report /tmp/benchmark-plan.yml \
  --output /tmp/benchmark-report.md
```

The report includes:

- the full `environment` table
- per-backend metric tables
- a Gate 12 coverage summary (which metrics are populated vs. still null)
- backend comparison highlights
- latency-tuning hints

To print the report to stdout instead of a file, omit `--output`.

---

## Complete example (copy-paste sequence)

```sh
# 1. Run automatic measurements
picoredsp-controller --run-benchmark --output /tmp/benchmark-plan.yml

# 2. (Optional) edit /tmp/benchmark-plan.yml to fill in manual fields

# 3. Validate
picoredsp-controller --validate-benchmark-plan /tmp/benchmark-plan.yml

# 4. Generate the report
picoredsp-controller --make-benchmark-report /tmp/benchmark-plan.yml \
  --output /tmp/benchmark-report.md

cat /tmp/benchmark-report.md
```

---

## Running the CI benchmark harness locally

CI runs a Rust benchmark suite that covers software-visible control-path
operations for both backends (aloop and ioplug).  These are useful for
detecting regressions in the controller itself but are not a substitute for
real hardware playback measurements.

```sh
cd /path/to/alsa_camilladsp_controller

# Native 64-bit Raspberry Pi / aarch64-unknown-linux-gnu hosts
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=gcc

# Build and run the Criterion/custom benchmark harness
cargo bench
```

Without that override, native AArch64 GNU systems can fail with
`linker 'aarch64-linux-gnu-gcc' not found` if the local machine only has the
host `gcc` installed.

The harness measures:

- `BenchmarkPlan` YAML serialization and deserialization round-trips
- `BenchmarkPlan` field-validation pass
- `aloop` backend: control-path snapshot diff and stream-event detection
- `ioplug` backend: IPC frame decode and HELLO version negotiation

Results are printed to stdout as `n / min / p50 / p95 / p99 / max / mean / stddev`.

---

## Limitations

- **True end-to-end latency** (analog output path, DAC pipeline) should be
  measured externally (e.g., with a loopback audio interface and a reference
  tone) because software-visible buffer depths do not capture the full DAC and
  analog output path.
- Rate-transition latencies depend on how quickly the audio source switches
  formats; they cannot be reproduced purely in software without a real format
  change event on the loopback.
- Soak and DAC-recovery tests require leaving the Pi running in a fixed
  configuration for extended periods; there is currently no automated harness
  for these.

For the full benchmark schema, field definitions, and CI verification commands,
see [BENCHMARK_FRAMEWORK.md](BENCHMARK_FRAMEWORK.md).
