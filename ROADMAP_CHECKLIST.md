# piCoreDSP Dual-Backend Architecture — Development Checklist

Generated from [`piCoreDSP_Dual_Backend_Roadmap.md`](piCoreDSP_Dual_Backend_Roadmap.md).
Each section maps to a milestone or gate. Check items off as work is completed.

---

## Gate 0 — Freeze current aloop baseline

**Milestone M0: Freeze current aloop baseline**

- [x] Document all current `snd-aloop` behaviour in a written acceptance specification
- [x] Define and write acceptance tests covering:
  - [x] Idle reboot
  - [x] First playback
  - [x] 44.1 → 48 → 96 kHz sample-rate changes
  - [x] Format changes
  - [x] Stop / start
  - [ ] GUI Apply and Save
  - [x] Active-config selection
  - [ ] PCP backup
  - [ ] Reboot persistence
  - [x] Controller restart
  - [x] CamillaDSP restart
  - [x] Transient WebSocket failure
- [x] All acceptance tests pass on the current codebase
- [ ] ✅ **Gate 0 passed**: aloop baseline is reproducible through the defined acceptance suite

---

## Gate 1 — Refactor Rust into backend-neutral core logic

**Milestone M1: Refactor Rust into backend-neutral core**

- [x] Define `StreamParams` struct (`rate`, `format`, `channels`)
- [x] Define `StreamEvent` enum (`Started`, `Changed`, `Stopped`)
- [x] Define `StreamBackend` trait (`next_event`)
- [ ] Separate all "stream detection" code from "what piCoreDSP does with a stream"
- [ ] Wrap existing HCTL code as `AloopBackend` implementing `StreamBackend`
- [ ] Add stub `IoplugBackend` placeholder (reads IPC, produces `StreamEvent`)
- [ ] Introduce target Rust module layout:
  - [ ] `core/` — state_machine, config, adaptation, persistence, errors, logging
  - [ ] `backend/` — mod, aloop, ioplug
  - [ ] `camilladsp/` — websocket, supervisor, alsa_capture, stdin_capture
  - [ ] `ipc/` — protocol, unix_socket
- [ ] ✅ **Gate 1 passed**: `backend=aloop` behaves identically to today — no regressions, no new features

**Milestone M2: Reimplement current aloop as backend module**

- [ ] Move all aloop-specific logic into `backend/aloop.rs`
- [ ] Remove backend-specific branching from the common core
- [ ] Validate no regressions against the Gate 0 acceptance suite

**Milestone M3: Establish identical behaviour / regression tests**

- [ ] Run the complete Gate 0 acceptance suite against the refactored code
- [ ] Add automated regression tests covering the same scenarios
- [ ] All tests green

---

## Phase 2 — Separate stream detection from audio transport

- [ ] Model detector and transport explicitly per backend:
  - [ ] Aloop: `detector = AloopHctl`, `transport = AlsaCapture`
  - [ ] ioplug: `detector = IoplugIpc`, `transport = StdinPipe`
- [ ] Remove all backend-specific branching from the common core

---

## Phase 3 — Config field ownership policy

- [ ] Define and document **user-owned** config fields (filters, mixers, pipeline, playback device, etc.)
- [ ] Define and document **runtime/backend-managed** config fields (samplerate, capture type/device/format/channels, stop_on_inactive, enable_rate_adjust)
- [ ] Verify that the same persistent DSP baseline config works with both backends
- [ ] Runtime config generation for `aloop` — injects ALSA capture section
- [ ] Runtime config generation for `ioplug` — injects Stdin capture section

---

## Gate 4 — Build standalone modern ALSA ioplug

**Milestone M4: Build standalone modern ALSA ioplug**

- [ ] Create new `picoredsp-ioplug/` project (do NOT continue the old `alsa_cdsp` source tree)
  - [ ] `src/pcm.c`
  - [ ] `src/ringbuffer.c` / `ringbuffer.h`
  - [ ] `src/ipc.c` / `ipc.h`
  - [ ] `src/timing.c`
  - [ ] `src/format.c`
  - [ ] `tests/`
  - [ ] `docs/BLUEALSA_TRACKING.md`
  - [ ] `CMakeLists.txt` or `Makefile`
- [ ] First prototype works without touching CamillaDSP (audio loopback/null sink only)

**Milestone M5: Validate ALSA ringbuffer / poll / XRUN semantics**

- [ ] Plugin loads as ALSA PCM
- [ ] hw_params negotiation works
- [ ] Plugin receives PCM
- [ ] Correct `hw_ptr` maintained
- [ ] Periods handled correctly
- [ ] Poll state reported correctly
- [ ] XRUN handled
- [ ] Pause / resume works
- [ ] Drain / drop works
- [ ] Close cleans up

---

## Phase 5 — BlueALSA reference review

- [ ] Review current BlueALSA PCM implementation vs. original `alsa_cdsp` fork point
- [ ] Document relevant learnings in `docs/BLUEALSA_TRACKING.md`:
  - [ ] C11 atomics usage
  - [ ] Ringbuffer pointer synchronisation
  - [ ] Period boundary handling
  - [ ] Buffer boundary handling
  - [ ] poll/revents behaviour
  - [ ] XRUN detection
  - [ ] Pause/resume synchronisation
  - [ ] Drain semantics
  - [ ] Thread cancellation
  - [ ] Signal masking
  - [ ] Delay accounting
  - [ ] alsa-lib compatibility workarounds
- [ ] Confirm no BlueALSA Bluetooth-specific code is copied (D-Bus, A2DP, SCO, ASHA, codec negotiation)

---

## Gate 6 — IPC protocol

**Milestone M6: Implement versioned plugin ↔ Rust IPC**

- [ ] Choose `AF_UNIX` socket as transport
- [ ] Define protocol version field from day one
- [ ] Define and implement all message types: `Hello`, `Start`, `Stop`, `Ready`, `Error`
- [ ] Define and document:
  - [ ] Endianness
  - [ ] Version negotiation
  - [ ] Unknown message handling
  - [ ] Disconnect behaviour
  - [ ] Timeouts
  - [ ] Maximum message length
  - [ ] Reconnect behaviour
  - [ ] Controller-unavailable behaviour
- [ ] Rust `PluginMessage` enum implemented in `ipc/protocol.rs`
- [ ] Rust IPC listener implemented in `ipc/unix_socket.rs`

---

## Gate 7 — START / READY handshake

**Milestone M7: Implement START / READY handshake**

- [ ] Plugin sends `START(rate, format, channels)` after `hw_params` negotiation
- [ ] Rust controller receives `START`, reads active baseline, validates, adapts runtime config, prepares CamillaDSP
- [ ] Rust controller sends `READY`
- [ ] Plugin releases PCM to CamillaDSP only after receiving `READY`
- [ ] Invariant enforced: no PCM transferred before `READY`

---

## Gate 8 — stdin PCM transport

**Milestone M8: Implement stdin pipe + FD handoff**

- [ ] Rust creates a `pipe()`
- [ ] Rust spawns CamillaDSP with pipe read fd as stdin
- [ ] Rust passes write fd to plugin over Unix socket using `SCM_RIGHTS`
- [ ] Plugin writes PCM directly into the fd (no Rust in the data path)
- [ ] Data path verified: Plugin → kernel pipe → CamillaDSP (never via Rust userspace)

**Milestone M9: Add Rust stdin CamillaDSP supervisor**

- [ ] Rust supervises CamillaDSP process lifecycle for ioplug backend:
  - [ ] Per-stream process model: `START → spawn → READY → PCM → stream ends → EOF → shutdown`
- [ ] ioplug backend reuses existing Rust recovery logic:
  - [ ] Validation failures
  - [ ] Transient failures + retry/backoff
  - [ ] Startup timeout
  - [ ] Process failure handling
  - [ ] Config fingerprint changes
  - [ ] Logging and state transitions
  - [ ] Shutdown
- [ ] C plugin does NOT implement policy (no retry logic, no config decisions)

---

## Gate 10 — Plugin failure model and functional test suite

**Milestone M10: Run complete ioplug functional suite**

Failure scenarios:
- [ ] Rust controller absent: plugin fails cleanly with meaningful ALSA error, no silent sample discard
- [ ] Invalid DSP config: `ERROR_CONFIG` returned, ALSA start fails cleanly
- [ ] CamillaDSP cannot open DAC: `ERROR_PLAYBACK_DEVICE` returned
- [ ] CamillaDSP exits mid-stream: plugin receives EPIPE, terminates ALSA stream cleanly, Rust records failure
- [ ] Plugin/application disappears: Rust cleans up CamillaDSP (control socket close + PCM fd close)
- [ ] Rust daemon restarts mid-stream: active stream fails cleanly (reconnect not required for v1)

Unit/integration tests:
- [ ] open/close
- [ ] hw_params negotiation
- [ ] Unsupported format / channels
- [ ] 44.1, 48, 88.2, 96, 176.4, 192 kHz
- [ ] Period wrap / buffer wrap
- [ ] Buffer size not divisible by period
- [ ] Partial write / EINTR / EPIPE
- [ ] Poll descriptors / poll revents
- [ ] Pause / resume / drain / drop / XRUN
- [ ] Rapid open/close / rapid format change
- [ ] Controller unavailable / controller timeout
- [ ] Invalid READY / protocol mismatch / socket disconnect
- [ ] CamillaDSP early exit / delayed startup / DAC unavailable

CI requirements:
- [ ] ASAN enabled
- [ ] UBSAN enabled
- [ ] TSAN enabled where practical
- [ ] clang and gcc warnings enabled
- [ ] Static analysis enabled
- [ ] Compiled with `-Wall -Wextra -Wpedantic -Werror` for all supported configurations

**Milestone M11: Run audio-integrity tests**

- [ ] Known PCM pattern sent through plugin → output captured → binary comparison
- [ ] All intended sample formats tested: S16_LE, S24_3LE, S24_4LE, S32_LE, F32_LE
- [ ] All intended sample rates tested
- [ ] No accidental resampling
- [ ] No accidental channel swap
- [ ] No byte-order error
- [ ] No 24-bit alignment error
- [ ] No truncation
- [ ] No gain modification
- [ ] No padding corruption
- [ ] ✅ Invariant established: ioplug transport is bit-transparent before CamillaDSP processing

---

## Phase 13 — Cross-backend Rust tests

- [ ] Rust tests decoupled from `snd-aloop` assumption
- [ ] Behavioural suite runs against abstract `StreamEvent` inputs:
  - [ ] `Started(44100, S16, 2)` → correct runtime config
  - [ ] `Changed(48000, S24, 2)` → correct restart/adaptation
  - [ ] `Stopped` → correct idle state
- [ ] Same suite runs for both `AloopBackend` and `IoplugBackend` event sources

---

## Gate 12 — A/B performance benchmarks

**Milestone M12: Run A/B latency and performance benchmarks**

- [ ] Benchmark framework established (same Pi / DAC / track / chunksize, only backend differs)
- [ ] All metrics measured for both `aloop` and `ioplug`:
  - [ ] Playback start latency
  - [ ] 44.1 → 48 kHz transition time
  - [ ] 48 → 96 kHz transition time
  - [ ] Stop latency
  - [ ] PCM transport latency
  - [ ] Total end-to-end latency (externally measured where possible)
  - [ ] CPU usage
  - [ ] Context switches
  - [ ] Controller RSS
  - [ ] Plugin overhead
  - [ ] XRUN count
  - [ ] 24h stability
  - [ ] 7-day stability
  - [ ] Recovery after DAC error

---

## Phase 16 — Latency tuning

> Only begin after correctness and stability are established.

Sequence: correctness → stability → measurement → latency optimisation

- [ ] Tune for each sample rate (44.1, 48, 96, 192 kHz) separately:
  - [ ] ALSA period size
  - [ ] ALSA buffer size
  - [ ] Plugin ringbuffer depth
  - [ ] Pipe size
  - [ ] CamillaDSP chunksize / queuelimit
  - [ ] DAC period/buffer parameters

---

## Gate 13 — Installer integration

**Milestone M13: Integrate both backends into installer**

- [ ] Installer installs both binaries:
  - [ ] `/usr/local/bin/picoredsp-controller`
  - [ ] `/usr/local/lib/alsa-lib/libasound_module_pcm_picoredsp.so`
- [ ] User-facing backend selection in installer (snd-aloop recommended/stable vs. direct ioplug experimental)
- [ ] Default = aloop
- [ ] Installer generates correct ALSA config for the selected backend
- [ ] Backend switch requires explicit restart/reboot (no dynamic in-stream switching)

## Phase 18 — Configuration migration

- [ ] Controller normalises any existing baseline config into the correct runtime config for the active backend
- [ ] Single `MySpeakers.yml` works with both backends (no separate copies needed)
- [ ] Capture section injected at runtime based on the active backend

---

## Phase 19 — BlueALSA upstream monitoring

- [ ] Create `docs/BLUEALSA_TRACKING.md` (or `docs/bluealsa-upstream.yml`)
- [ ] Record: repository, tracked source files, last reviewed commit, review date, relevant topic categories
- [ ] CI automation detects new relevant BlueALSA changes and opens a GitHub issue (never auto-merges)
- [ ] Review process established: detect → issue → manual review → relevant? → port concept/fix or mark reviewed

## Phase 20 — alsa-lib monitoring

- [ ] alsa-lib version tracked separately from BlueALSA
- [ ] New alsa-lib release triggers the plugin test suite automatically
- [ ] Maintenance priority documented:
  - [ ] HIGH: alsa-lib, CamillaDSP, piCorePlayer
  - [ ] MEDIUM: Linux ALSA, BlueALSA reference changes
  - [ ] LOW: unrelated BlueALSA Bluetooth functionality

---

## Gate 14 — Experimental release

**Milestone M14: Experimental real-hardware release**

- [ ] ioplug released as `backend=ioplug, status=experimental`; aloop remains `status=recommended`
- [ ] Tested on:
  - [ ] Multiple Raspberry Pi generations
  - [ ] Multiple DACs
  - [ ] Long-running playback
  - [ ] Frequent sample-rate changes
  - [ ] AirPlay
  - [ ] Bluetooth
  - [ ] Squeezelite
  - [ ] GUI editing
  - [ ] Reboots
  - [ ] Controller restarts
  - [ ] CamillaDSP failures

**Milestone M15: Long-term field testing**

- [ ] All M14 scenarios stable over extended real-world usage
- [ ] No PCM corruption
- [ ] No significant crash regressions
- [ ] No unexplained XRUN regressions

---

## Gate 16 — Production promotion decision

**Milestone M16: Decide default backend**

ioplug production-readiness criteria:
- [ ] No PCM corruption
- [ ] No significant crash regressions
- [ ] No unexplained XRUN regressions
- [ ] Correct format handling
- [ ] Correct sample-rate handling
- [ ] Reliable pause/stop/start
- [ ] Reliable CamillaDSP cleanup
- [ ] Reliable GUI persistence
- [ ] Reliable reboot behaviour
- [ ] Clean controller failure handling
- [ ] Long-duration stability demonstrated

Measurable benefit demonstrated in at least one area:
- [ ] Lower latency, OR
- [ ] Faster rate switching, OR
- [ ] Simpler runtime architecture, OR
- [ ] Lower CPU/context-switch cost, OR
- [ ] Better determinism

Decision:
- [ ] Outcome decided: **A** (aloop default, ioplug optional) / **B** (ioplug default, aloop fallback) / **C** (both first-class, user chooses)

---

## Architectural guard rails (verify throughout all phases)

These must hold at every stage of development:

- [ ] Working aloop backend never removed early
- [ ] PCM never routed through the Rust daemon
- [ ] Complete `bluealsa-pcm.c` never copied wholesale
- [ ] BlueALSA never made a runtime dependency
- [ ] Config/persistence logic never duplicated in C plugin
- [ ] C plugin never decides policy
- [ ] BlueALSA changes never auto-cherry-picked
- [ ] No dynamic aloop ↔ ioplug switching while audio is running
- [ ] No separate DSP configs per backend unless unavoidable
- [ ] Latency not optimised before correctness and stability are established
