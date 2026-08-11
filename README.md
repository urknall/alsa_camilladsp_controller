# piCoreCDSP — snd-aloop + Rust CamillaDSP controller for piCorePlayer

## Overview

This repository contains:

| Path | Purpose |
|------|---------|
| `install_picoredsp.sh` | Installer script — run once on the piCorePlayer device |
| `src/` | Rust source for `picoredsp-controller` |
| `Cargo.toml` | Rust package manifest |
| `.github/workflows/build.yml` | GitHub Actions CI/CD — builds and releases binaries |

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

The installer:
1. Tests `snd-aloop` availability.
2. Detects the physical DAC selected in Squeezelite Settings.
3. Downloads the pre-built `picoredsp-controller` binary from GitHub Releases.
4. Downloads CamillaDSP and CamillaGUI backend binaries.
5. Generates default/bypass/null configs and ALSA PCM definitions.
6. Bundles everything into a Tiny Core `.tcz` extension.
7. Routes Squeezelite through `pcm.picoredsp` and reboots.

After reboot, CamillaGUI is accessible at `http://pcp.local:5000`.

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

`.github/workflows/build.yml` runs in two modes:

- Pushes to `main` run tests, cross-build both ARM binaries, and refresh the rolling `installer-latest` GitHub release used by the installer.
- Pull requests run tests and cross-build both ARM binaries as GitHub Actions artifacts for CI verification.
- Pushing a `v*.*.*` tag additionally creates an immutable GitHub Release and attaches both binaries.

The installer downloads `picoredsp-controller` from the rolling `installer-latest` GitHub release, not from workflow artifacts.
Pushes to `main` refresh that release, while version tags still create immutable `v*.*.*` releases for manual versioned downloads.

## Diagnostics

```sh
# Probe snd-aloop controls (requires snd-aloop loaded):
picoredsp-controller --probe --device hw:Loopback,0

# Check CamillaDSP WebSocket connectivity:
picoredsp-controller --ws-check --host 127.0.0.1 --port 1234

# Dry-run YAML adaptation, write to stdout:
picoredsp-controller --adapt-check \
  --adapt /mnt/mmcblk0p2/tce/camilladsp/active_config.yml \
  --rate 48000 --format S32_LE --channels 2
```

## References

- [HEnquist/camilladsp](https://github.com/HEnquist/camilladsp)
- [HEnquist/camilladsp-controller](https://github.com/HEnquist/camilladsp-controller)
- [HEnquist/camillagui-backend](https://github.com/HEnquist/camillagui-backend)
- [JWahle/piCoreCDSP](https://github.com/JWahle/piCoreCDSP) — original ALSA cdsp-plugin based implementation
