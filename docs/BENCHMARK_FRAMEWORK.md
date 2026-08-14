# Benchmark framework

Milestone M12 requires an A/B benchmark setup where the Raspberry Pi, piCorePlayer version, CamillaDSP version, DAC, DSP configuration, reference track, chunksize, and queuelimit stay fixed while only the backend changes between `aloop` and `ioplug`.

The controller now provides two helper commands for that workflow:

```sh
# Generate a canonical benchmark plan template.
cargo run -- --make-benchmark-plan --output /tmp/benchmark-plan.yml

# Validate a filled-in plan before collecting measurements.
cargo run -- --validate-benchmark-plan /tmp/benchmark-plan.yml
```

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
