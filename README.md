# piCoreCDSP — snd-aloop + Rust CamillaDSP controller for piCorePlayer

## Overview

This repository contains:

| Path | Purpose |
|------|---------|
| `install_picoredsp.sh` | Installer script — run once on the piCorePlayer device |
| `src/` | Rust source for `picoredsp-controller` |
| `Cargo.toml` | Rust package manifest |
| `.github/workflows/build.yml` | GitHub Actions CI/CD — builds and releases binaries |

## Supported architectures

| Architecture | Boards |
|---|---|
| `aarch64` | Raspberry Pi 3/4/5 running 64-bit piCorePlayer |
| `armv7` | Raspberry Pi 2/3 running 32-bit piCorePlayer |

## Audio path

```
Squeezelite / AirPlay / Bluetooth
              │
              ▼
        pcm.picoredsp          ← ALSA plug PCM defined by installer
              │
              ▼
       hw:Loopback,1,0
              │
          snd-aloop             ← kernel module
              │
              ▼
       hw:Loopback,0,0
              │
              ▼
          CamillaDSP            ← DSP engine
              │
              ▼
             DAC
```

> **Note:** The `pcm.picoredsp` routing is fixed to **stereo (2 channels)**. Only
> stereo audio sources are supported. The controller rejects ALSA events that
> indicate a channel count other than 2.

The `picoredsp-controller` binary sits entirely outside the audio path. It
monitors the `snd-aloop` ALSA HCTL controls (`PCM Slave Active`, `PCM Slave
Rate`, `PCM Slave Format`, `PCM Slave Channels`) and drives CamillaDSP through
its WebSocket API (`GetState`, `GetStopReason`, `SetConfig`, `Stop`).

## Installation

```sh
# On piCorePlayer, as user tc:
wget https://github.com/urknall/alsa_camilladsp_controller/raw/main/install_picoredsp.sh
chmod +x install_picoredsp.sh
./install_picoredsp.sh
```

The installer performs these steps in order:

1. Tests `snd-aloop` availability.
2. Downloads and SHA256-verifies the pre-built `picoredsp-controller` binary from GitHub Releases.
3. Probes the snd-aloop ALSA controls with the downloaded binary.
4. Detects the physical DAC selected in Squeezelite Settings.
5. Downloads and SHA256-verifies CamillaDSP and CamillaGUI backend binaries.
6. Generates default/bypass/null configs and ALSA PCM definitions.
7. Validates all staged configs with `camilladsp --check`.
8. Bundles everything into a Tiny Core `.tcz` extension.
9. Routes Squeezelite through `pcm.picoredsp` and reboots.

After reboot, CamillaGUI is accessible at `http://pcp.local:5000`.

### Troubleshooting: `syntax error: unexpected newline`

If you see `./install_picoredsp.sh: line N: syntax error: unexpected newline` immediately
after running the script, the most likely cause is a **stale or corrupt download**.
Some older piCorePlayer `wget` builds cache redirect responses without following them,
leaving an HTML error page saved as the script file.

Fix:

```sh
rm -f install_picoredsp.sh
wget -O install_picoredsp.sh \
  https://raw.githubusercontent.com/urknall/alsa_camilladsp_controller/main/install_picoredsp.sh
chmod +x install_picoredsp.sh
sh -n install_picoredsp.sh && echo "syntax OK"
./install_picoredsp.sh
```

The `sh -n` dry-run will report any remaining parse error before the script actually runs.

## CamillaGUI — Apply without Save

CamillaGUI lets you click **Apply** to send a configuration directly to CamillaDSP
without clicking **Save**.  When that happens, CamillaDSP is running an in-memory
config that is not written to disk.

The controller uses the file behind the `active_config.yml` symlink as its source
of truth.  On the next sample-rate change (or controller restart) it re-reads that
file, which will be the previously saved version — **not** the unsaved in-memory
config.

**Recommendation:** always click **Save** (or **Apply and Save**) in CamillaGUI
before relying on a config change to survive a playback format switch or reboot.

## Baseline config vs. live runtime config

piCoreDSP stores the selected YAML file as a persistent baseline configuration.

The Rust `picoredsp-controller` monitors `snd-aloop` and adapts the configuration
in memory when playback starts or the stream format changes. The runtime sample rate,
capture format and channel count can therefore differ from the values stored in the
YAML file.

For example, a config file may contain:

    samplerate: 44100

while CamillaGUI's live status shows:

    48000 Hz

This is expected. The live CamillaGUI status reflects the stream currently processed
by CamillaDSP and is authoritative for current runtime parameters.

Runtime adaptations are not written back to the YAML file.

Use **Apply and Save** for DSP or filter changes that must persist across sample-rate
changes or reboots. On piCorePlayer, run:

    pcp backup

after persistent configuration changes and before rebooting, so the current system
configuration is backed up.

Do not manually use **Apply** just to initialize DSP while playback is stopped.
The controller automatically loads and adapts the configuration when playback
becomes active.


## Migrating legacy custom configs

Older piCorePlayer/CamillaDSP configs often use a different transport model
(e.g. `capture.type: Stdin`, fixed capture/playback formats, and fixed transport
parameters in the YAML). The current piCoreDSP setup routes audio through
`snd-aloop` and treats the YAML as a persistent **DSP baseline** while the Rust
controller adapts live stream parameters in memory.

For the complete old-vs-new flow, config field ownership policy, field-by-field
migration rules, a recommended `devices:` block, CamillaGUI active-file semantics,
and the `pcp backup` workflow, see [CONFIG_MIGRATION.md](CONFIG_MIGRATION.md).

## Controller modules

| Module | Contents |
|--------|---------|
| `src/error.rs` | `AppResult<T>` type alias and `app_error()` helper |
| `src/logging.rs` | `LogLevel` enum and `log()` function |
| `src/wave.rs` | `WaveFormat` and `DeviceSnapshot` structs |
| `src/alsa_listener.rs` | `AlsaLoopbackListener`, ALSA format → CamillaDSP mapping |
| `src/adapt.rs` | `adapt_config()` — YAML adaptation logic |
| `src/camilla_ws.rs` | `CamillaWs` client, reply/state/stop-reason parsing |
| `src/controller.rs` | `Controller` — main control loop |
| `src/args.rs` | CLI argument parsing, `Mode` enum |
| `src/main.rs` | Entry point, mode dispatch |

## CI/CD

`.github/workflows/build.yml` runs when Rust/controller-related files change (`src/**`, `Cargo.toml`, `Cargo.lock`, `install_picoredsp.sh`, workflow file itself), plus manual dispatch and version tags:

- Pushes to `main` run tests, cross-build both ARM binaries, and refresh the rolling `installer-latest` GitHub release used by the installer.
- Pull requests run tests and cross-build both ARM binaries as GitHub Actions artifacts for CI verification.
- Pushing a `v*.*.*` tag additionally creates an immutable GitHub Release and attaches both binaries. The CI enforces that the tag matches the `version` field in `Cargo.toml`.

The installer downloads `picoredsp-controller` from the rolling `installer-latest` GitHub release, not from workflow artifacts.
Pushes to `main` refresh that release, while version tags still create immutable `v*.*.*` releases for manual versioned downloads.

## Diagnostics

### Prerequisites

Before running any diagnostic command you need the `picoredsp-controller` binary available on `$PATH` (or provide the full path).  The installer places it at `/usr/local/bin/picoredsp-controller`, so after a successful install the binary is already present.

If you have **not yet run the installer** (e.g. you want to diagnose the system first), download the binary manually:

```sh
# On piCorePlayer, as user tc:

# 1. Detect your architecture (uname -m returns armv7l on 32-bit; releases use armv7)
ARCH=$(uname -m)
case "$ARCH" in armv7l) ARCH=armv7 ;; esac

# 2. Download the pre-built binary from the rolling release
wget -O picoredsp-controller \
  "https://github.com/urknall/alsa_camilladsp_controller/releases/download/installer-latest/picoredsp-controller-${ARCH}"
chmod +x picoredsp-controller

# 3. Load snd-aloop if it is not already loaded (required for --probe)
sudo modprobe snd-aloop
```

Each diagnostic command has its own runtime prerequisite:

| Command | What must be running / loaded |
|---------|-------------------------------|
| `--probe` | `snd-aloop` kernel module loaded (`lsmod \| grep snd_aloop`) |
| `--ws-check` | CamillaDSP process running and listening on the given host:port |
| `--adapt-check` | The `active_config.yml` file (or symlink target) must exist on disk |
| `--make-benchmark-plan` / `--validate-benchmark-plan` | No runtime dependencies |

### Diagnostic commands

```sh
# Probe snd-aloop controls (requires snd-aloop loaded):
picoredsp-controller --probe --device hw:Loopback,0

# Check CamillaDSP WebSocket connectivity:
picoredsp-controller --ws-check --host 127.0.0.1 --port 1234

# Dry-run YAML adaptation, write to stdout:
picoredsp-controller --adapt-check \
  --adapt /mnt/mmcblk0p2/tce/camilladsp/active_config.yml \
  --rate 48000 --format S32_LE --channels 2

# Generate and validate the A/B benchmark plan used for roadmap milestone M12:
picoredsp-controller --make-benchmark-plan --output /tmp/benchmark-plan.yml
picoredsp-controller --validate-benchmark-plan /tmp/benchmark-plan.yml
picoredsp-controller --make-benchmark-report /tmp/benchmark-plan.yml --output /tmp/benchmark-report.md
```

See [docs/BENCHMARK_FRAMEWORK.md](docs/BENCHMARK_FRAMEWORK.md) for the benchmark plan schema and validation rules.
See [docs/MEASUREMENTS.md](docs/MEASUREMENTS.md) for the end-to-end walkthrough of running automatic measurements and generating a report.

## Benchmarking and full local verification

The benchmark CLI provides a **plan generator**, **plan validator**, and
**benchmark report generator**. CI also runs the Rust benchmark harness with
automated `aloop` and `ioplug` control-path microbenchmarks.

For **local development on native 64-bit Raspberry Pi /
`aarch64-unknown-linux-gnu` systems**, use the host linker for Cargo commands:

```sh
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=gcc
```

This local-only override applies to `cargo build`, `cargo test`, `cargo run`,
and `cargo bench`.
Without it, a native AArch64 GNU system can fail with
`linker 'aarch64-linux-gnu-gcc' not found` if only the host `gcc` is installed.
CI cross-builds do **not** use this override; the workflow explicitly sets the
cross-linker back to `aarch64-linux-gnu-gcc`.

Use it to create a reproducible A/B benchmark record where only the backend changes:

```sh
cd /home/runner/work/alsa_camilladsp_controller/alsa_camilladsp_controller

cargo run -- --make-benchmark-plan --output /tmp/benchmark-plan.yml
cargo run -- --validate-benchmark-plan /tmp/benchmark-plan.yml
cargo run -- --make-benchmark-report /tmp/benchmark-plan.yml --output /tmp/benchmark-report.md
```

After validation, run the `aloop` and `ioplug` backends under the same Pi /
piCorePlayer / CamillaDSP / DAC / DSP config / track / chunksize / queuelimit
conditions, fill in the resulting metrics in the YAML plan, and regenerate the
report to get Gate 12 coverage, backend comparisons, and latency-tuning hints.

To run the same full local verification suite that CI uses for the Rust
controller:

```sh
cd /home/runner/work/alsa_camilladsp_controller/alsa_camilladsp_controller

sudo apt-get install -y libasound2-dev
# Only on native 64-bit Raspberry Pi / aarch64-unknown-linux-gnu hosts:
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=gcc
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
sh -n install_picoredsp.sh
dash -n install_picoredsp.sh
```

Optional MSRV check (matches CI's Rust 1.71 compile gate):

```sh
cd /home/runner/work/alsa_camilladsp_controller/alsa_camilladsp_controller
cargo +1.71 check --locked
```

The automated benchmark harness now covers software-visible comparison points in
CI: the Rust benchmark suite measures `aloop` and `ioplug` control-path
operations and emits an automated benchmark report artifact. Hardware playback,
rate-transition, soak, and end-to-end latency measurements still need a real Pi
/ DAC test rig, and true end-to-end latency should still be externally measured
where possible rather than inferred purely from software-visible buffers.

## References

- [HEnquist/camilladsp](https://github.com/HEnquist/camilladsp)
- [HEnquist/camilladsp-controller](https://github.com/HEnquist/camilladsp-controller)
- [HEnquist/camillagui-backend](https://github.com/HEnquist/camillagui-backend)
- [JWahle/piCoreCDSP](https://github.com/JWahle/piCoreCDSP) — original ALSA cdsp-plugin based implementation
