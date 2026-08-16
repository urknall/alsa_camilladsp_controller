# Benchmark framework

Milestone M12 requires an A/B benchmark setup where the Raspberry Pi, piCorePlayer version, CamillaDSP version, DAC, DSP configuration, reference track, chunksize, and queuelimit stay fixed while only the backend changes between `aloop` and `ioplug`.

The controller now provides two helper commands for that workflow:

```sh
# Generate a canonical benchmark plan template.
cargo run -- --make-benchmark-plan --output /tmp/benchmark-plan.yml

# Validate a filled-in plan before collecting measurements.
cargo run -- --validate-benchmark-plan /tmp/benchmark-plan.yml

# Render a benchmark report with Gate 12 coverage and tuning hints.
cargo run -- --make-benchmark-report /tmp/benchmark-plan.yml --output /tmp/benchmark-report.md
```

## Current status

These commands create, validate, and summarize the benchmark record format.
They do **not** currently:

- start or stop playback automatically
- generate synthetic audio input
- simulate sample-rate transitions
- run long-duration stability loops
- collect timing / CPU / RSS / XRUN metrics on their own

Today, the filled benchmark YAML is the reproducible container for those results,
but the measurements themselves still need to be produced by a separate test
harness or manual test workflow.

## What the template enforces

The generated YAML contains:

- one shared `environment` block for the fixed test setup
- exactly two runs: one `aloop`, one `ioplug`
- the required sample-rate set:
  - 44100
  - 48000
  - 96000
  - 192000
- placeholders for all roadmap metrics:
  - playback start latency
  - 44.1 → 48 kHz transition time
  - 48 → 96 kHz transition time
  - stop latency
  - PCM transport latency
  - total end-to-end latency
  - CPU usage
  - context switches
  - controller RSS
  - plugin overhead
  - XRUN count
  - 24h stability
  - 7-day stability
  - recovery after DAC error

## Validation rules

`--validate-benchmark-plan` fails unless:

- `version` matches the current benchmark schema
- all shared environment strings are non-empty
- `chunksize` and `queuelimit` are greater than zero
- `sample_rates_hz` is exactly `[44100, 48000, 96000, 192000]`
- the run list contains exactly one `aloop` run and one `ioplug` run

This establishes a reproducible benchmark scaffold before real hardware measurements are added.

CI now also runs a software-visible benchmark harness for both backends:

- `aloop` control-path snapshot/event detection
- `ioplug` IPC decode / handshake control-path operations
- benchmark-plan serialization and validation overhead

Those CI measurements are useful for controller-side regressions and automated
reports, but they are not a replacement for real Pi / DAC playback latency data.

## Running local verification alongside benchmark work

The benchmark plan is separate from the controller's normal correctness checks.
To run the local Rust/controller subset of CI plus the installer syntax gate:

```sh
cd /path/to/alsa_camilladsp_controller

sudo apt-get install -y libasound2-dev
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
sh -n install_picoredsp.sh
dash -n install_picoredsp.sh
```

Separate CI jobs also cover live CamillaDSP compatibility, native
`picoredsp-ioplug/` C builds/tests, sanitizers, clang-tidy, and ARM packaging.

Optional MSRV gate:

```sh
cd /path/to/alsa_camilladsp_controller
cargo +1.71 check --locked
```

## Scope for the hardware benchmark harness

Yes — a fuller automated hardware benchmark runner can be built, but it is a
larger feature than the current schema/reporting support.

A useful harness would:

- drive both backends under the same fixed benchmark plan
- generate deterministic audio input
- automate start/stop and rate-transition scenarios
- run soak tests for 24-hour and 7-day stability
- record host-side metrics into the benchmark YAML automatically
- regenerate the markdown benchmark report after each measurement pass

Examples of metrics that are good candidates for automatic collection:

- playback start latency
- stop latency
- 44.1 → 48 kHz and 48 → 96 kHz transition timing
- CPU usage
- context switches
- controller RSS
- plugin overhead
- XRUN count
- pass/fail stability and DAC-recovery outcomes

One important limitation remains: total end-to-end latency should still be
externally measured where possible, because software-visible queue depths do not
fully capture the DAC and analog output path.
