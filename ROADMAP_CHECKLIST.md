# piCoreDSP Dual-Backend Architecture — Development Checklist

Generated from [`piCoreDSP_Dual_Backend_Roadmap.md`](piCoreDSP_Dual_Backend_Roadmap.md).
Each section maps to a milestone or gate. Check items off as work is completed.

> **Checklist honesty pass (2026-08):** an independent architecture review
> found drift in both directions — items checked without meeting their
> stated bar, and items left unchecked despite being implemented. Phase 5
> (BlueALSA review) and the Gate 10 `ERROR_PLAYBACK_DEVICE` item were
> reopened because they weren't actually true yet; Phase 18 and Gate 13's
> parent box were checked off because the underlying work already existed.
> See inline notes below each affected item, and the correctness-fix plan
> tracked alongside this checklist for the remediation work in progress.
> Phase 5 was subsequently re-closed on 2026-08-15 (Step 7 of that plan)
> after an actual review of current upstream BlueALSA source was performed
> and documented.
> As Step 9 of the same plan (2026-08-15), the six leftover re-export shim
> modules (`src/adapt.rs`, `src/alsa_listener.rs`, `src/camilla_ws.rs`,
> `src/error.rs`, `src/logging.rs`, `src/wave.rs`) were deleted and their
> three callers repointed at the real `core::*`/`camilladsp::*` modules, and
> the 1728-line `src/benchmark.rs` was split into a `src/benchmark/` module
> folder (`report.rs`, `runner.rs`, `parsing.rs`, `collectors.rs`,
> `measurement.rs`, `mod.rs`) with its test suite divided across the new
> files by concern. No architectural checklist gate covered this cleanup;
> it is recorded here for traceability. All 213 tests continued to pass
> throughout, and `cargo fmt`/`cargo clippy --all-targets` are clean.
> Step 10 (2026-08-15, hygiene): added a top-level `LICENSE` file (MIT,
> matching `Cargo.toml`'s existing `license = "MIT"`) and refreshed
> `Cargo.toml`'s `description` to name both backends instead of only
> `snd-aloop`.
>
> **Second honesty pass (2026-08-15, "verify against real code, not the
> checklist"):** a follow-up review was re-run against current source with a
> full toolchain available (cargo/rustc/clippy, gcc/clang/cmake/ctest, and a
> downloaded real CamillaDSP 4.1.3 release binary) rather than trusting this
> file's prior notes. Most of the re-raised correctness claims (SIGPIPE
> handling, STOP ordering, drain timeout, pause/resume sync, `sw_params`
> /`avail_min`, DAC-open error classification, benchmark WebSocket command
> names, BlueALSA tracking currency, upstream-tracking automation, LICENSE/
> `Cargo.toml` hygiene) were confirmed **already fixed** by reading the
> current code directly. Two genuinely new, previously-unverified bugs were
> found by validating generated configs against the real CamillaDSP binary
> (something no prior pass had done) and fixed:
> 1. `build_runtime_capture()` in `src/core/adaptation.rs` wrote the
>    `Alsa`-schema format name `S24_4_LE` into the `ioplug` backend's
>    generic `Stdin` capture block. CamillaDSP 4.1.3 uses two different
>    format enums depending on device type — `Alsa` devices accept
>    `S24_4_LE` but reject `S24_4_RJ_LE`/`S24_4_LJ_LE`, while generic
>    devices (`Stdin`, `File`, ...) accept `S24_4_RJ_LE`/`S24_4_LJ_LE` but
>    reject `S24_4_LE` — confirmed with `camilladsp --check` against real
>    4.1.3. Every ioplug stream using a 24-bit-in-32-bit container format
>    would have been rejected by CamillaDSP at startup. Fixed by adding
>    `alsa_only_format_to_generic()` (`src/camilladsp/alsa_capture.rs`),
>    applied only to the ioplug `Stdin` capture path.
> 2. The `portable_base_config()` Rust test fixture used the obsolete
>    CamillaDSP pipeline field `channel: 0` (singular) instead of the v4.1.3
>    schema's `channels: [0]` (list) — only caught because this pass
>    actually validated the fixture against the real binary instead of only
>    round-tripping it through serde.
>
> Two `#[ignore]`-gated tests were added, opt-in via
> `PICOREDSP_TEST_CAMILLADSP_BIN`, that validate generated configs and
> WebSocket queries against a real CamillaDSP binary and would have caught
> both regressions:
> `core::adaptation::tests::ioplug_adapted_config_validates_against_real_camilladsp_for_all_formats`
> and
> `benchmark::collectors::tests::live_collectors_work_against_real_camilladsp`.
> CI now runs these in two new `build.yml` jobs: `camilladsp_compat_pinned`
> (blocking, tests the exact pinned `CDSP_VERSION`) and
> `camilladsp_compat_latest` (non-blocking, tests upstream's current
> `latest` release as an early-warning signal ahead of intentionally
> bumping the pin).
>
> Claims from the same review that remain genuinely open (not addressed in
> this pass, scoped out as separate, higher-risk mechanical refactors rather
> than attempted under time pressure): `src/core/persistence.rs` is still a
> placeholder (statefile logic remains in `core::adaptation`), and
> `state_machine.rs` (1840 lines), `core/adaptation.rs` (1613 lines) and
> `backend/ioplug.rs` (1022 lines) have not been split into submodules the
> way `benchmark.rs` was in Step 9. `test_audio_integrity.c` still exercises
> the transport path directly rather than through a live CamillaDSP process
> end-to-end (the M11/Phase-13 critique). These are recorded here rather
> than silently dropped.

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
  - [x] GUI Apply and Save
  - [x] Active-config selection
  - [x] PCP backup
  - [x] Reboot persistence
  - [x] Controller restart
  - [x] CamillaDSP restart
  - [x] Transient WebSocket failure
- [x] All acceptance tests pass on the current codebase
- [x] ✅ **Gate 0 passed**: aloop baseline is reproducible through the defined acceptance suite

---

## Gate 1 — Refactor Rust into backend-neutral core logic

**Milestone M1: Refactor Rust into backend-neutral core**

- [x] Define `StreamParams` struct (`rate`, `format`, `channels`)
- [x] Define `StreamEvent` enum (`Started`, `Changed`, `Stopped`)
- [x] Define `StreamBackend` trait (`next_event`)
- [x] Separate all "stream detection" code from "what piCoreDSP does with a stream"
- [x] Wrap existing HCTL code as `AloopBackend` implementing `StreamBackend`
- [x] Add stub `IoplugBackend` placeholder (reads IPC, produces `StreamEvent`)
- [x] Introduce target Rust module layout:
  - [x] `core/` — state_machine, config, adaptation, persistence, errors, logging
  - [x] `backend/` — mod, aloop, ioplug
  - [x] `camilladsp/` — websocket, supervisor, alsa_capture, stdin_capture
  - [x] `ipc/` — protocol, unix_socket
- [x] ✅ **Gate 1 passed**: `backend=aloop` behaves identically to today — no regressions, no new features

**Milestone M2: Reimplement current aloop as backend module**

- [x] Move all aloop-specific logic into `backend/aloop.rs`
- [x] Remove backend-specific branching from the common core
- [x] Validate no regressions against the Gate 0 acceptance suite

**Milestone M3: Establish identical behaviour / regression tests**

- [x] Run the complete Gate 0 acceptance suite against the refactored code
- [x] Add automated regression tests covering the same scenarios
- [x] All tests green

---

## Phase 2 entry gate — Quality checks before continuing

- [x] Run `cargo fmt` and apply formatting fixes
- [x] Run `cargo clippy --fix` and resolve remaining warnings/errors
- [x] Run the full test suite
- [x] Fix failing tests and re-run format, Clippy, and tests until all checks pass
- [x] Continue with Phase 2 only after all checks are green

---

## Phase 2 — Separate stream detection from audio transport

- [x] Model detector and transport explicitly per backend:
  - [x] Aloop: `detector = AloopHctl`, `transport = AlsaCapture`
  - [x] ioplug: `detector = IoplugIpc`, `transport = StdinPipe`
- [x] Localize backend-specific branching to the adaptation layer
  - Note: `src/core/adaptation.rs` contains a `match backend { Aloop => … Ioplug => … }` block
    for capture-policy generation; this is intentional and correctly scoped to the one place
    where the two backends differ.  The rest of the common core is backend-agnostic.

---

## Phase 3 — Config field ownership policy

- [x] Define and document **user-owned** config fields (filters, mixers, pipeline, playback device, etc.)
- [x] Define and document **runtime/backend-managed** config fields (samplerate, capture type/device/format/channels, stop_on_inactive, enable_rate_adjust)
- [x] Verify that the same persistent DSP baseline config works with both backends
- [x] Runtime config generation for `aloop` — injects ALSA capture section
- [x] Runtime config generation for `ioplug` — injects Stdin capture section

---

## Gate 4 — Build standalone modern ALSA ioplug

**Milestone M4: Build standalone modern ALSA ioplug**

- [x] Create new `picoredsp-ioplug/` project (do NOT continue the old `alsa_cdsp` source tree)
  - [x] `src/pcm.c`
  - [x] `src/ringbuffer.c` / `ringbuffer.h`
  - [x] `src/ipc.c` / `ipc.h`
  - [x] `src/timing.c`
  - [x] `src/format.c`
  - [x] `tests/`
  - [x] `docs/BLUEALSA_TRACKING.md`
  - [x] `CMakeLists.txt` or `Makefile`
- [x] First prototype works without touching CamillaDSP (audio loopback/null sink only)

**Milestone M5: Validate ALSA ringbuffer / poll / XRUN semantics**

- [x] Plugin loads as ALSA PCM
- [x] hw_params negotiation works
- [x] Plugin receives PCM
- [x] Correct `hw_ptr` maintained
- [x] Periods handled correctly
- [x] Poll state reported correctly
- [x] XRUN handled
- [x] Pause / resume works
- [x] Drain / drop works
- [x] Close cleans up

---

## Phase 5 — BlueALSA reference review

> Closed out 2026-08-15 (Step 7 of the correctness follow-up plan):
> `picoredsp-ioplug/docs/BLUEALSA_TRACKING.md` now records an actual review
> of current upstream `src/asound/bluealsa-pcm.c` at commit `84ad90d`
> (2026-08-15), including the corrected tracked-path layout and a finding
> that BlueALSA has no separate ring-buffer file (contrary to the earlier
> "design intent" notes, which assumed one). No `alsa_cdsp` code was ever
> imported into this project; piCoreDSP's ioplug was written from scratch
> against ALSA's public `pcm_ioplug.h`, so "vs. original `alsa_cdsp` fork
> point" is reframed as "vs. current upstream BlueALSA reference" — see the
> tracking doc for details.
>
> **Exact-mechanism follow-up (2026-08-15/16):** a further pass re-checked
> the reviewer's incomplete-topics table against BlueALSA's *exact*
> mechanism, not just its outcome, for two rows that were previously
> "functionally equivalent but different in mechanism":
> 1. **Signal masking** — the worker thread previously blocked only
>    `SIGPIPE` (`sigaddset`+`SIG_BLOCK`). Changed to block the full signal
>    set via `sigfillset()` + `pthread_sigmask(SIG_SETMASK, ...)`, matching
>    BlueALSA's `io_thread_setup()` exactly. Renamed
>    `pcdsp_worker_block_sigpipe()` → `pcdsp_worker_block_all_signals()`
>    (`pcm_worker.c`/`pcm_worker.h`). New regression test
>    `block_all_signals_blocks_full_signal_set_not_just_sigpipe`
>    (`test_pcm_worker.c`) captures the worker thread's own signal mask and
>    proves `SIGUSR1`/`SIGTERM`/`SIGHUP` are blocked in addition to
>    `SIGPIPE`.
> 2. **Drain timeout** — `pcdsp_drain()` previously used a flat 5 s
>    constant. Replaced with `pcdsp_drain_timeout_ns()` (`pcm.c`), computing
>    BlueALSA's own formula `100ms + periods_remaining * period_time` from
>    the frames actually queued at drain-entry. New regression test
>    `drain_timeout_scales_with_backlog_not_flat_constant`
>    (`test_pcm_integration.c`) measures wall-clock `-ETIMEDOUT` latency for
>    a small vs. large period/buffer configuration and asserts the bound
>    scales with backlog (>2× difference) rather than being fixed.
>
> Full C suite re-run after both changes: 7/7 binaries, 0 failures
> (`cmake --build . -j$(nproc) && ctest --output-on-failure`). `cargo test`
> re-run to confirm the Rust side is unaffected: 214 passed, 0 failed.
> `BLUEALSA_TRACKING.md`'s verification table and prose sections updated to
> cite the new code and tests.

- [x] Review current BlueALSA PCM implementation as an engineering reference (no `alsa_cdsp` fork exists in this project to diff against)
- [x] Document relevant learnings in `picoredsp-ioplug/docs/BLUEALSA_TRACKING.md`:
  - [x] C11 atomics usage
  - [x] Ringbuffer pointer synchronisation
  - [x] Period boundary handling
  - [x] Buffer boundary handling
  - [x] poll/revents behaviour
  - [x] XRUN detection
  - [x] Pause/resume synchronisation
  - [x] Drain semantics
  - [x] Thread cancellation
  - [x] Signal masking
  - [x] Delay accounting
  - [x] alsa-lib compatibility workarounds
- [x] Confirm no BlueALSA Bluetooth-specific code is copied (D-Bus, A2DP, SCO, ASHA, codec negotiation)

---

## Gate 6 — IPC protocol

**Milestone M6: Implement versioned plugin ↔ Rust IPC**

- [x] Choose `AF_UNIX` socket as transport
- [x] Define protocol version field from day one
- [x] Define and implement all message types: `Hello`, `Start`, `Stop`, `Ready`, `Error`
- [x] Define and document:
  - [x] Endianness
  - [x] Version negotiation
  - [x] Unknown message handling
  - [x] Disconnect behaviour
  - [x] Timeouts
  - [x] Maximum message length
  - [x] Reconnect behaviour
  - [x] Controller-unavailable behaviour
- [x] Rust `PluginMessage` enum implemented in `ipc/protocol.rs`
- [x] Rust IPC listener implemented in `ipc/unix_socket.rs`

---

## Gate 7 — START / READY handshake

**Milestone M7: Implement START / READY handshake**

- [x] Plugin sends `START(rate, format, channels)` after `hw_params` negotiation
- [x] Rust controller receives `START`, reads active baseline, validates, adapts runtime config, prepares CamillaDSP
- [x] Rust controller sends `READY`
- [x] Plugin releases PCM to CamillaDSP only after receiving `READY`
- [x] Invariant enforced: no PCM transferred before `READY`

---

## Gate 8 — stdin PCM transport

**Milestone M8: Implement stdin pipe + FD handoff**

- [x] Rust creates a `pipe()`
- [x] Rust spawns CamillaDSP with pipe read fd as stdin
- [x] Rust passes write fd to plugin over Unix socket using `SCM_RIGHTS`
- [x] Plugin writes PCM directly into the fd (no Rust in the data path)
- [x] Data path verified: Plugin → kernel pipe → CamillaDSP (never via Rust userspace)

**Milestone M9: Add Rust stdin CamillaDSP supervisor**

- [x] Rust supervises CamillaDSP process lifecycle for ioplug backend:
  - [x] Per-stream process model: `START → spawn → READY → PCM → stream ends → EOF → shutdown`
- [x] ioplug backend reuses existing Rust recovery logic:
  - [x] Validation failures
  - [x] Transient failures + retry/backoff
  - [x] Startup timeout
  - [x] Process failure handling
  - [x] Config fingerprint changes
  - [x] Logging and state transitions
  - [x] Shutdown
- [x] C plugin does NOT implement policy (no retry logic, no config decisions)

---

## Gate 10 — Plugin failure model and functional test suite

**Milestone M10: Run complete ioplug functional suite**

Failure scenarios:
- [x] Rust controller absent: plugin fails cleanly with meaningful ALSA error, no silent sample discard
- [x] Invalid DSP config: `ERROR_CONFIG` returned, ALSA start fails cleanly
- [x] CamillaDSP cannot open DAC: `ERROR_PLAYBACK_DEVICE` returned
  <br>Fixed: `run_ioplug()`'s startup-check failure branch now classifies an
  immediate CamillaDSP exit via `classify_early_exit_error()`, which inspects
  the captured stderr tail (`StdinPipeProcess`/`StdinSupervisor::recent_stderr()`)
  for CamillaDSP's `"Playback error: ..."` marker (see upstream `src/bin.rs`).
  A playback-device failure now returns `ErrorCode::PlaybackDevice` and is
  retried transiently (exponential backoff via `retry.record_attempt()`)
  instead of being latched permanently under the config-fingerprint policy;
  a genuine config/DSP-graph failure still returns `ErrorCode::Config` and
  latches as before. See Step 5 of the correctness follow-up plan.
- [x] CamillaDSP exits mid-stream: plugin receives EPIPE, terminates ALSA stream cleanly, Rust records failure
  <br>Fixed: the worker thread now blocks `SIGPIPE` for itself (`pthread_sigmask`,
  thread-scoped) instead of relying on tests installing process-wide `SIG_IGN`;
  see `pcdsp_worker_block_sigpipe()` in `pcm_worker.c` and the regression test
  `worker_survives_default_sigpipe_disposition_via_thread_scoped_block`.
- [x] Plugin/application disappears: Rust cleans up CamillaDSP (control socket close + PCM fd close)
- [x] Rust daemon restarts mid-stream: active stream fails cleanly (reconnect not required for v1)

Unit/integration tests:
- [x] open/close
- [x] hw_params negotiation
- [x] Unsupported format / channels
- [x] 44.1, 48, 88.2, 96, 176.4, 192 kHz
- [x] Period wrap / buffer wrap
- [x] Buffer size not divisible by period
- [x] Partial write / EINTR / EPIPE
- [x] Poll descriptors / poll revents
- [x] Pause / resume / drain / drop / XRUN
- [x] Rapid open/close / rapid format change
- [x] Controller unavailable / controller timeout
- [x] Invalid READY / protocol mismatch / socket disconnect
- [x] CamillaDSP early exit / delayed startup / DAC unavailable

CI requirements:
- [x] ASAN run in CI (`asan` job in `build.yml`, ASAN=ON, clang, `detect_leaks=1`)
- [x] UBSAN run in CI (`ubsan` job in `build.yml`, UBSAN=ON, clang, `halt_on_error=1`)
- [x] TSAN run in CI where practical (`tsan` job in `build.yml`, TSAN=ON, clang, `halt_on_error=1`)
- [x] clang and gcc warnings enabled in the native ioplug CTest job
- [x] Static analysis run in CI (`clang_tidy` job in `build.yml`, CLANG_TIDY=ON, `--warnings-as-errors=*`)
- [x] Compiled with `-Wall -Wextra -Wpedantic -Werror` for the native GCC/Clang test configurations

**Milestone M11: Run audio-integrity tests**

> Note: these tests exercise `pcdsp_drain_period_to_pipe()` / the worker's
> ring-buffer → pipe write path directly (`test_audio_integrity.c`), not the
> full `application → ioplug → pipe → CamillaDSP → DAC` chain. The invariant
> below is scoped accordingly: it proves the ioplug's own transport is
> bit-transparent, not that CamillaDSP-side processing is transparent too.

- [x] Known PCM pattern sent through plugin → output captured → binary comparison
- [x] All intended sample formats tested: S16_LE, S24_3_LE, S24_4LE, S32_LE, F32_LE
- [x] All intended sample rates tested
- [x] No accidental resampling
- [x] No accidental channel swap
- [x] No byte-order error
- [x] No 24-bit alignment error
- [x] No truncation
- [x] No gain modification
- [x] No padding corruption
- [x] ✅ Invariant established: ioplug transport is bit-transparent before CamillaDSP processing

---

## Phase 13 — Cross-backend Rust tests

- [x] Rust tests decoupled from `snd-aloop` assumption
- [x] Behavioural suite runs against abstract `StreamEvent` inputs:
  - [x] `Started(44100, S16, 2)` → correct runtime config
  - [x] `Changed(48000, S24, 2)` → correct restart/adaptation
  - [x] `Stopped` → correct idle state
- [x] Same suite runs for both `AloopBackend` and `IoplugBackend` event sources

---

## Gate 12 — A/B performance benchmarks

**Milestone M12: Run A/B latency and performance benchmarks**

- [x] Benchmark framework established (same Pi / DAC / track / chunksize, only backend differs)
  <br>Fixed (Step 6): `src/benchmark.rs` previously queried CamillaDSP over
  WebSocket with `GetSamplerate`/`GetBuffersize`, neither of which exist in
  CamillaDSP 4.1.3 (every measurement silently collapsed to `None`/default).
  Replaced with `GetCaptureRate` (measured sample rate) and
  `GetConfigValue`/`/devices/chunksize` (configured buffer size), extracted
  into testable helpers (`buffer_latency_ms_from_client`,
  `chunksize_from_client`) with unit tests pinning the exact command names
  sent. Also fixed stale CamillaDSP format-string test fixtures
  (`S24_3LE`→`S24_3_LE`, `FLOAT_LE`→`F32_LE`) in `src/backend.rs`,
  `src/backend/aloop.rs`, `src/core/state_machine.rs`.
- [x] Automated benchmark harness created and integrated into CI:
  - [x] C microbenchmark harness (`picoredsp-ioplug/bench/`) covering ring buffer, timing, PCM worker
  - [x] Rust benchmark runner (`benches/picoredsp_bench.rs`) covering YAML serialisation / plan validation
  - [x] Rust benchmark runner covers both `aloop` and `ioplug` control-path microbenchmarks
  - [x] Benchmark report generator creates automated Gate 12 coverage and comparison reports
  - [x] CI jobs `ioplug_bench` and `rust_bench` build and run benchmarks on every push
  - [x] CI jobs `camilladsp_compat_pinned` (blocking) and `camilladsp_compat_latest`
    (non-blocking) run the live-CamillaDSP `#[ignore]`d tests against the
    exact pinned `CDSP_VERSION` and against upstream's current `latest`
    release respectively (added 2026-08-15, second honesty pass)
- [ ] All metrics measured for both `aloop` and `ioplug`:
  - [x] Playback start latency (auto-collected for aloop via HCTL polling)
  - [ ] 44.1 → 48 kHz transition time (requires manual rate-switch test)
  - [ ] 48 → 96 kHz transition time (requires manual rate-switch test)
  - [x] Stop latency (auto-collected for aloop via HCTL polling)
  - [x] PCM transport latency (auto-collected from `/proc/asound` hw_params)
  - [x] Total end-to-end latency (auto-computed: transport + CamillaDSP buffer from WS)
  - [x] CPU usage (auto-collected from `/proc/<pid>/stat`)
  - [x] Context switches (auto-collected from `/proc/<pid>/status`)
  - [x] Controller RSS (auto-collected from `/proc/<pid>/status`)
  - [ ] Plugin overhead (requires CPU comparison with DSP bypassed vs. active)
  - [x] XRUN count (auto-collected from aplay stderr during timing test)
  - [ ] 24h stability (requires manual soak run)
  - [ ] 7-day stability (requires manual soak run)
  - [ ] Recovery after DAC error (requires deliberate hardware fault injection)

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

- [x] Installer installs both binaries:
  - [x] `/usr/local/bin/picoredsp-controller`
  - [x] `/usr/local/lib/alsa-lib/libasound_module_pcm_picoredsp.so`
- [x] User-facing backend selection in installer (snd-aloop recommended/stable vs. direct ioplug experimental)
- [x] Default = aloop
- [x] Installer generates correct ALSA config for the selected backend
- [x] Backend switch requires explicit restart/reboot (no dynamic in-stream switching)

## Phase 18 — Configuration migration

> Checked off: implemented by `adapt_config_for_backend()` in
> `src/core/adaptation.rs` (dispatches on `RuntimeBackend::Aloop` /
> `RuntimeBackend::Ioplug` to inject the correct capture section) and
> documented in `CONFIG_MIGRATION.md`. Verified by the
> `same_portable_baseline_adapts_for_aloop_and_ioplug` test, which asserts a
> single baseline config adapts correctly for both backends.
>
> **2026-08-15 fix:** the ioplug capture section was writing the
> `Alsa`-schema format name (`S24_4_LE`) into the generic `Stdin` capture
> block, which real CamillaDSP 4.1.3 rejects (see honesty-pass note above).
> Fixed via `alsa_only_format_to_generic()`; now covered by a live-binary
> test (`ioplug_adapted_config_validates_against_real_camilladsp_for_all_formats`).

- [x] Controller normalises any existing baseline config into the correct runtime config for the active backend
- [x] Single `MySpeakers.yml` works with both backends (no separate copies needed)
- [x] Capture section injected at runtime based on the active backend

---

## Phase 19 — BlueALSA upstream monitoring

> Closed out 2026-08-15 (Step 8 of the correctness follow-up plan):
> `docs/upstream-tracking.yml` is the machine-readable manifest and
> `.github/workflows/upstream-tracking.yml` + `scripts/check_upstream_tracking.py`
> implement the detect → issue automation, scheduled weekly.

- [x] Create `picoredsp-ioplug/docs/BLUEALSA_TRACKING.md` and a `bluealsa` entry in `docs/upstream-tracking.yml`
- [x] Record: repository, tracked source files, last reviewed commit, review date, relevant topic categories
- [x] CI automation detects new relevant BlueALSA changes and opens a GitHub issue (never auto-merges)
- [x] Review process established: detect → issue → manual review → relevant? → port concept/fix or mark reviewed

## Phase 20 — alsa-lib monitoring

> Closed out 2026-08-15: `docs/ALSA_LIB_TRACKING.md` + the `alsa-lib` entry
> in `docs/upstream-tracking.yml`.

- [x] alsa-lib version tracked separately from BlueALSA
- [x] New alsa-lib release triggers the plugin test suite automatically (see `docs/ALSA_LIB_TRACKING.md` Automation section)
- [x] Maintenance priority documented:
  - [x] HIGH: alsa-lib, CamillaDSP, piCorePlayer
  - [x] MEDIUM: Linux ALSA, BlueALSA reference changes
  - [x] LOW: unrelated BlueALSA Bluetooth functionality

## Phase 20a — CamillaDSP upstream monitoring

> Closed out 2026-08-15: `docs/CAMILLADSP_TRACKING.md` + the `camilladsp`
> entry in `docs/upstream-tracking.yml`.

- [x] Create `docs/CAMILLADSP_TRACKING.md` (and `docs/upstream-tracking.yml` machine-readable entry)
- [x] Record: repository URL, last reviewed tag/commit, review date, relevant topic categories
- [x] CI automation detects new relevant CamillaDSP changes (websocket API, config schema, process lifecycle, stdin transport) and opens a GitHub issue (label: `upstream/camilladsp`; never auto-merges)
- [x] Review process established: detect → issue → manual review → relevant? → update Rust websocket client / lifecycle handling + regression test, or mark reviewed

## Phase 20b — camilladsp-controller upstream monitoring

> Closed out 2026-08-15: `docs/CAMILLADSP_CONTROLLER_TRACKING.md` + the
> `camilladsp-controller` entry in `docs/upstream-tracking.yml`.

- [x] Create `docs/CAMILLADSP_CONTROLLER_TRACKING.md` (and `docs/upstream-tracking.yml` machine-readable entry)
- [x] Record: repository URL, last reviewed tag/commit, review date, relevant topic categories
- [x] CI automation detects new relevant camilladsp-controller changes (command names, response parsing, state machine, new/deprecated commands) and opens a GitHub issue (label: `upstream/camilladsp-controller`; never auto-merges)
- [x] Review process established: detect → issue → manual review → relevant? → update Rust websocket client + protocol test, or mark reviewed

## Phase 20c — CamillaDSP GUI upstream monitoring

> Closed out 2026-08-15: `docs/CAMILLAGUI_TRACKING.md` + the
> `camillagui-backend` / `camillagui` entries in `docs/upstream-tracking.yml`.

- [x] Create `docs/CAMILLAGUI_TRACKING.md` (and `docs/upstream-tracking.yml` machine-readable entries)
- [x] Record: backend + frontend repository URLs, last reviewed tag/commit, review date, relevant topic categories
- [x] CI automation detects new relevant camillagui / camillagui-backend changes (websocket API calls, config schema, volume/device API) and opens a GitHub issue (label: `upstream/camillagui`; never auto-merges)
- [x] Review process established: detect → issue → manual review → relevant? → update Rust websocket client / config schema handling + regression test, or mark reviewed


---

## Gate 14 — Experimental release

**Milestone M14: Experimental real-hardware release**

Field test log: [`docs/GATE14_FIELD_TEST_LOG.md`](docs/GATE14_FIELD_TEST_LOG.md)

- [ ] ioplug released as `backend=ioplug, status=experimental`; aloop remains `status=recommended`
- [ ] Tested on:
  - [ ] Multiple Raspberry Pi generations
  - [ ] Multiple DACs
  - [ ] Long-running playback (Scenario 12 — 24 h stability)
  - [ ] Frequent sample-rate changes (Scenario 2 + 11)
  - [ ] AirPlay (Scenario 3)
  - [ ] Bluetooth (Scenario 4)
  - [ ] Squeezelite (Scenario 1)
  - [ ] GUI editing (Scenario 10)
  - [ ] Reboots (Scenario 5)
  - [ ] Controller restarts (Scenario 6)
  - [ ] CamillaDSP failures (Scenario 7)

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
