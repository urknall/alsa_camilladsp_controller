# Migrating legacy piCorePlayer / CamillaDSP configs to piCoreDSP

This document explains how older piCorePlayer/CamillaDSP configurations differ from the current piCoreDSP architecture, which fields should be changed, which values remain persistent baseline values, and which parameters are now determined dynamically at runtime.

The most important conceptual change is:

> **A saved YAML file is the persistent DSP baseline. It is no longer necessarily an exact snapshot of the parameters currently used by CamillaDSP.**

The Rust `picoredsp-controller` monitors the active backend (`snd-aloop` HCTL
for `aloop`, ioplug IPC for `ioplug`) and adapts the configuration in memory
before starting/restarting CamillaDSP.

## Config field ownership policy

The persistent YAML is meant to hold the user's DSP design, while transport-specific
capture details are determined by piCoreDSP at runtime.

### User-owned fields

These fields should remain part of the persistent baseline config and should be
edited intentionally by the user:

- `filters`
- `mixers`
- `processors`
- `pipeline`
- `devices.playback.*` (including the physical playback device)
- `devices.capture.labels`
- intentional gains, delays, crossovers, routing choices, and FIR coefficient references
- deliberately tuned buffering values such as `devices.chunksize` and `devices.queuelimit`

### Runtime/backend-managed fields

These fields describe the active transport and should be treated as runtime values
managed by piCoreDSP rather than as fixed user tuning knobs:

- `devices.samplerate`
- `devices.capture.type`
- `devices.capture.device`
- `devices.capture.format`
- `devices.capture.channels`
- `devices.capture.stop_on_inactive`
- `devices.enable_rate_adjust`

### Backend transport details

#### Shared baseline rules

Regardless of backend, the saved YAML should describe the DSP design and any
intentional persistent buffering choices, while piCoreDSP injects the active
transport details at runtime.

#### `aloop` transport (`backend=aloop`)

For `aloop`, piCoreDSP derives the runtime capture values from the live ALSA
loopback stream and builds the runtime CamillaDSP configuration in memory.

#### `ioplug` transport (`backend=ioplug`)

For `ioplug`, the controller receives stream parameters from the native plugin's
AF_UNIX IPC handshake, injects a `Stdin` capture block for the runtime config,
strips ALSA-only capture keys that CamillaDSP's `Stdin` device does not accept,
and then spawns/supervises CamillaDSP for that stream. The same saved DSP
baseline therefore remains portable across both backends.

## Old vs. new audio flow

### Legacy setup

Older configurations often captured audio from stdin and therefore encoded more transport details directly in the YAML file:

```text
Squeezelite / source
        │
        ▼
      stdin
        │
        ▼
   CamillaDSP
   capture = Stdin
   samplerate = fixed value
   capture format = fixed value
        │
        ▼
       DAC
```

A typical legacy `devices:` block looked similar to:

```yaml
devices:
  samplerate: 44100
  chunksize: 2048
  capture:
    type: Stdin
    channels: 2
    format: S16_LE
  playback:
    type: Alsa
    channels: 2
    device: plughw:CARD=sndrpihifiberry,DEV=0
    format: S32_LE
```

In that model, the config file itself described both the DSP and much of the transport format.

### Current piCoreDSP setup — `aloop` transport

piCoreDSP routes all supported sources through an ALSA loopback device:

```text
Squeezelite / AirPlay / Bluetooth
              │
              ▼
        pcm.picoredsp
              │
              ▼
       hw:Loopback,1,0
              │
          snd-aloop
              │
              ▼
       hw:Loopback,0,0
              │
              ▼
          CamillaDSP
              │
              ▼
             DAC
```

The Rust controller is **outside the PCM audio path**:

```text
snd-aloop HCTL controls
 Active / Rate / Format / Channels
              │
              ▼
    picoredsp-controller
              │
       WebSocket control
              │
              ▼
          CamillaDSP
```

When playback becomes active, the controller reads the live stream parameters, re-reads the selected baseline YAML, adapts a runtime copy in memory, and sends that adapted config to CamillaDSP.

### Current piCoreDSP setup — `ioplug` transport

The same baseline YAML can also run through the direct ioplug transport:

```text
Squeezelite / AirPlay / Bluetooth
              │
              ▼
        pcm.picoredsp
              │
              ▼
libasound_module_pcm_picoredsp.so
              │
              ▼
      AF_UNIX + stdin pipe
              │
              ▼
    picoredsp-controller
              │
              ▼
          CamillaDSP
              │
              ▼
             DAC
```

In this mode the plugin reports rate / format / channels to the controller,
which adapts the selected baseline YAML into a runtime `Stdin` capture config
without writing those transport fields back into the saved file.

## Migration table

### `aloop` transport (`backend=aloop`)

| Legacy field / behaviour | Current piCoreDSP recommendation | Why |
|---|---|---|
| `capture.type: Stdin` | `capture.type: Alsa` | Audio now enters CamillaDSP through `snd-aloop`, not stdin. |
| No capture device | `capture.device: hw:Loopback,0,0` | This is the CamillaDSP capture side of the loopback pair. |
| Fixed `capture.format` | Usually omit it | The stream format is runtime-dependent. Avoid making an old fixed transport format part of the persistent baseline unless there is a deliberate reason to constrain it. |
| Fixed `devices.samplerate` interpreted as live rate | Keep a valid value such as `44100`, but treat it as a **baseline** | `picoredsp-controller` replaces/adapts the runtime samplerate to the active stream before starting CamillaDSP. |
| No `stop_on_inactive` | `stop_on_inactive: true` | CamillaDSP must release the loopback capture side when playback becomes inactive so a later stream can establish a new rate/format cleanly. The Rust controller additionally enforces the inactive-source invariant. |
| No `enable_rate_adjust` | `enable_rate_adjust: true` | Rate adjustment handles small long-term clock drift while a stream is running. It is separate from sample-rate switching. |
| Fixed `playback.format` such as `S32_LE` | Usually omit it unless intentionally required | Playback format does not need to be hard-coded merely because the old config used one. Keep it only when the DAC or processing setup requires a specific format. |
| `channels: 2` | Keep `2` | The installed `pcm.picoredsp` path is intentionally stereo and the controller rejects non-stereo loopback events. |
| Physical DAC device | Keep it only if it still matches the current DAC | The safest reference is `devices.playback.device` from the current generated `Bypass.yml`. |
| DSP filters/mixers/processors/pipeline | Keep unchanged | These are the actual persistent DSP design and are independent of the transport migration. |

### `ioplug` transport (`backend=ioplug`)

| Legacy field / behaviour | Current piCoreDSP recommendation | Why |
|---|---|---|
| `capture.type: Alsa` or `capture.type: Stdin` in the saved baseline | Keep the baseline transport-light; the runtime config will use `capture.type: Stdin` automatically | The ioplug backend injects the active capture transport when it launches CamillaDSP. |
| `capture.device: hw:Loopback,0,0` | Remove it from backend-portable baselines | The runtime `Stdin` capture block has no ALSA device field. |
| `capture.stop_on_inactive: true` | Do not rely on it in the saved baseline | `stop_on_inactive` is an ALSA-capture setting; the ioplug runtime capture omits it. |
| ALSA-only capture keys such as `link_mute_control` / `link_volume_control` | Remove them if you want the same YAML to validate on both backends | The controller strips ALSA-only capture keys before spawning CamillaDSP with `capture.type: Stdin`. |
| Fixed `capture.format` copied from old ALSA configs | Usually omit it from the baseline | The ioplug runtime chooses the active stream format, converting ALSA-only names to the generic CamillaDSP `Stdin` equivalents when required. |

## Recommended current `devices:` baseline

For a normal stereo custom config targeting `backend=aloop`, the device section
should typically look like this:

```yaml
devices:
  samplerate: 44100
  chunksize: 2048
  queuelimit: 4
  enable_rate_adjust: true

  capture:
    type: Alsa
    channels: 2
    device: hw:Loopback,0,0
    stop_on_inactive: true
    labels:
      - Input_L
      - Input_R

  playback:
    type: Alsa
    channels: 2
    device: plughw:CARD=sndrpihifiberry,DEV=0
```

`playback.device` above is only an example. Use the physical DAC device from
the current `Bypass.yml` generated on the target piCorePlayer installation. For
`backend=ioplug`, keep the same playback side and DSP structure, but expect the
runtime capture block to be rewritten to `type: Stdin` with no loopback device.

## Baseline values vs. live runtime values

A custom YAML may contain:

```yaml
samplerate: 44100
```

while a 96 kHz track is playing and CamillaGUI shows:

```text
Capt. samplerate: 96000
```

This is expected.

```text
YAML on disk          44100   persistent baseline
ALSA live stream      96000   source reality
Controller runtime    96000   adapted in-memory config
CamillaDSP live       96000   actual running DSP state
```

For the currently playing stream, the **live CamillaGUI status is authoritative**. Runtime-adapted values are deliberately not written back into the user's YAML file. On the ioplug backend the controller writes the adapted copy to a transient runtime file under `/run/picoredsp/`, not back through `active_config.yml`.

This separation prevents every 44.1/48/96 kHz transition from rewriting the persistent DSP configuration and keeps user DSP settings separate from transient transport parameters.

## Why the player must open snd-aloop first

The `snd-aloop` pair must agree on rate, format and channel parameters. The first side that establishes the stream parameters constrains the paired side.

For this reason piCoreDSP starts CamillaDSP with `--wait --no_config` and does **not** load a capture config while playback is idle.

The intended startup sequence is:

```text
reboot
  │
  ▼
CamillaDSP --wait --no_config
  │
  ▼
source inactive -> no SetConfig
  │
  ▼
player starts and opens snd-aloop first
  │
  ▼
controller reads live rate / format / channels
  │
  ▼
controller adapts selected YAML in memory
  │
  ▼
SetConfig -> CamillaDSP opens capture second
```

This is why a manual **Apply** while playback is stopped is not needed merely to initialize the DSP. The controller automatically loads the selected baseline when playback becomes active.

## `enable_rate_adjust` is not sample-rate switching

These two mechanisms solve different problems:

```text
picoredsp-controller
    44100 -> 48000 -> 96000
    stream-rate / format changes

CamillaDSP rate adjust
    tiny clock differences while one stream is running
    keeps the buffer from slowly filling or emptying
```

Do not remove `enable_rate_adjust: true` simply because the controller already switches sample rates.

## CamillaGUI: current file vs. active file

CamillaGUI has two separate concepts:

1. the file currently loaded/edited in the GUI;
2. the file marked as the persistent **active config** in the Files tab.

**Apply and Save does not automatically mark a file as the active config.**

For a custom config that should be restored after reboot, use this workflow:

```text
create/load custom config
        │
        ▼
Files tab -> mark it active with the star
        │
        ▼
edit DSP / filters
        │
        ▼
Apply and Save
        │
        ▼
pcp backup
        │
        ▼
reboot
```

The installer integration keeps the persistent active selection through `active_config.yml`, and the Rust controller re-reads that selected YAML whenever it needs to build a new runtime config.

If the star still points to `Bypass.yml`, then `Apply and Save` on another file will save that file, but `Bypass.yml` remains the persistent active baseline for the next reboot.

## piCorePlayer persistence

After changing persistent configuration, run:

```sh
pcp backup
```

before rebooting piCorePlayer.

A useful rule is:

- **Apply**: change the current CamillaDSP runtime only.
- **Save / Apply and Save**: persist DSP changes in the selected YAML file.
- **Star / Set active**: choose which YAML is the persistent active baseline.
- **`pcp backup`**: persist the piCorePlayer system state before reboot.

## What should remain in custom configs

The actual audio tuning belongs in the YAML and should normally be migrated unchanged:

- filters and coefficient references;
- mixers and routing matrices;
- processors;
- pipeline order;
- intentional gains, delays and crossovers;
- labels;
- chunksize/queue choices when deliberately tuned;
- the physical playback device.

The migration is mainly about removing old assumptions from the **capture/transport layer**, not rewriting the DSP design.

## Quick migration checklist

For each old custom YAML:

- [ ] Choose the backend you intend to run first (`aloop` or `ioplug`).
- [ ] For `backend=aloop`: change `capture.type` to `Alsa`.
- [ ] For `backend=aloop`: set `capture.device: hw:Loopback,0,0`.
- [ ] For `backend=aloop`: set `capture.channels: 2`.
- [ ] For `backend=aloop`: add `capture.stop_on_inactive: true`.
- [ ] For `backend=ioplug`: remove ALSA-only capture fields you do not want in a portable baseline (`capture.device`, `capture.stop_on_inactive`, `link_*` controls).
- [ ] Remove an old fixed `capture.format` unless it is intentionally required.
- [ ] Keep a valid `devices.samplerate` baseline such as `44100`.
- [ ] Add/keep `enable_rate_adjust: true`.
- [ ] Remove an old fixed `playback.format` unless the DAC requires it.
- [ ] Verify `devices.playback.device` against the current generated `Bypass.yml`.
- [ ] Leave filters/mixers/processors/pipeline unchanged unless intentionally editing the DSP.
- [ ] Validate the migrated YAML in CamillaGUI.
- [ ] Mark the desired file active with the star.
- [ ] Use **Apply and Save** for persistent DSP edits.
- [ ] Run `pcp backup` before reboot.

## Summary

The legacy model treated the YAML as both DSP configuration and a mostly fixed description of the transport format.

The current piCoreDSP model separates those responsibilities:

```text
persistent YAML
    DSP design + baseline

        +

live ALSA state
    rate / format / channels

        ↓

picoredsp-controller
    runtime adaptation

        ↓

CamillaDSP
    actual live configuration
```

This separation is what allows one custom DSP configuration to follow changing source rates without rewriting the user's YAML on every track change.
