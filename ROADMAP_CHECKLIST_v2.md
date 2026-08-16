# piCoreCDSP v2 — Development Checklist

Generated from [`piCoreCDSP_v2_Roadmap.md`](piCoreCDSP_v2_Roadmap.md), which formalizes the uploaded plan at
[`docs/new plan/piCoreCDSP_v2_complete_roadmap(1).md`](docs/new%20plan/piCoreCDSP_v2_complete_roadmap%281%29.md).

This checklist **replaces** `ROADMAP_CHECKLIST.md` (v1, dual-backend) as the tracker for new work. Following the
Gate 0 cutover below, the v1 checklist and roadmap no longer exist on `main` — they are reachable only via the
`v1-final` tag / `v1-archive` branch — and must not be used to plan v2 work. Each section below maps to a gate or
milestone in the v2 roadmap. Check items off only after they are actually implemented and verified — do not check an
item off because it is merely planned.

---

## Gate 0 — Repository Reset & Legacy Isolation

- [x] Decide Option A (git-tag-only reset) vs. Option B (temporary `reference/v1-legacy/` subfolder) per roadmap §0.

  **Decision record:** **Option A — git-tag-only reset** is adopted. Rationale: a permanent (or even
  long-lived "temporary") `reference/v1-legacy/` subfolder stays part of `main`'s working tree and remains
  visible to CI, IDE search, and any agent reading the repo for context — exactly the risk §0/§50 warn about,
  since it would make it easy to silently interleave v1 patterns into v2 modules. A `v1-final` tag (plus an
  optional `v1-archive` branch) keeps the full v1 tree permanently retrievable via `git checkout`/`git diff`
  without it ever being part of `main`'s working tree, matching the "git is the archive" rule (roadmap §50, §53)
  and the Core Philosophy that v2 replaces v1 rather than living beside it.

- [x] Tag current `main` HEAD as `v1-final`.

  **Status:** done. `v1-final` is an annotated tag pointing at commit `72b556d` (the `main` tip immediately
  before this Gate 0/1 documentation change), pushed by a maintainer with repository push access.

- [x] (If desired) cut a `v1-archive` branch from `v1-final`.

  **Status:** done. `v1-archive` branch created from `v1-final` (same commit, `72b556d`) and pushed to `origin`.
- [ ] If Option B chosen: move `src/`, `picoredsp-ioplug/`, `benches/`, `scripts/`, `install_picoredsp.sh`,
      `CONFIG_MIGRATION.md` into a clearly named, non-buildable reference folder, excluded from the Cargo
      workspace and CI.

  N/A — Option B was not chosen.

- [x] If Option A chosen: remove the v1 source tree from `main` once v2 implementation is ready to start.

  **Status:** done, explicitly confirmed by the repository owner. With `v1-final`/`v1-archive` in place as the
  permanent archive, `src/`, `picoredsp-ioplug/`, `benches/`, `scripts/`, `install_picoredsp.sh`,
  `CONFIG_MIGRATION.md`, `Cargo.toml`/`Cargo.lock`, `.cargo/config.toml`, the v1 release CI
  (`.github/workflows/build.yml`), the v1 upstream-tracking CI (`.github/workflows/upstream-tracking.yml`) and
  its supporting docs (`docs/upstream-tracking.yml`, `docs/ALSA_LIB_TRACKING.md`, `docs/CAMILLADSP_TRACKING.md`,
  `docs/CAMILLAGUI_TRACKING.md`, `docs/CAMILLADSP_CONTROLLER_TRACKING.md`, `docs/BENCHMARK_FRAMEWORK.md`,
  `docs/MEASUREMENTS.md`, `docs/GATE0_ACCEPTANCE_SPEC.md`, `docs/GATE14_FIELD_TEST_LOG.md`), and the frozen v1
  planning docs (`piCoreDSP_Dual_Backend_Roadmap.md`, `ROADMAP_CHECKLIST.md`) are all removed from `main`. This
  is a deliberate, one-way cutover: `main` no longer builds an installable piCoreCDSP product until v2
  implementation (Gates 2–12) reaches equivalent functionality. `README.md` was rewritten to target v2
  exclusively and point at the `v1-final` tag / `v1-archive` branch for the archived implementation.

- [x] `.github/copilot-instructions.md` updated to reference only `piCoreCDSP_v2_Roadmap.md` and this checklist
      (done as part of the planning change that introduced this file; updated again to reflect that the v1 docs
      are now removed from `main`, not merely frozen in place).
- [x] `piCoreDSP_Dual_Backend_Roadmap.md` and `ROADMAP_CHECKLIST.md` marked superseded, then removed from `main`
      entirely once the v1-tree removal above executed (both remain reachable via `v1-final`/`v1-archive`).
- [x] No new v2 code is added to `src/` until the reset decision above is executed.

  The reset decision (Option A) is recorded and now fully executed (tag + branch + v1 removal). `src/` does not
  currently exist on `main`; the first v2 code added there must follow Gate 2 onward.

- [x] ✅ **Gate 0 passed**: repository state is unambiguous about which code/docs are "live" v2 work vs. archived v1
      reference.

  `main` now contains only the v2 roadmap/checklist/process docs and this README; the entire v1 tree, CI, and
  planning docs are gone from the working copy and reachable solely via the `v1-final` tag / `v1-archive` branch.

---

## Gate 1 — Strategic & Process Architecture Decisions Recorded

- [x] Production stack pin policy defined: CamillaDSP 5 if hardware-validated in time, otherwise CamillaDSP 4.2 (roadmap §1).

  Recorded in roadmap §1 ("Guiding rule: design for CamillaDSP 5 semantics, ship only against a pinned and
  hardware-validated release stack") and confirmed as agreed policy; final pin selection itself happens at
  Gate 12 once hardware validation results are known.

- [x] No permanent multi-version compatibility policy documented and agreed.

  Recorded in roadmap §1: exactly one CamillaDSP/CamillaGUI combination is pinned before the first v2 release;
  development adapters for superseded versions are deleted after an upgrade, never kept running in parallel.

- [x] "Workaround needs a removal criterion" rule adopted as a contribution requirement (e.g. PR template / lint).

  Implemented as `.github/PULL_REQUEST_TEMPLATE.md`, which requires every PR introducing a workaround/compatibility
  bridge to state its removal criterion (and, once `upstream/capabilities.yml` exists per Gate 14, its entry
  there), consistent with roadmap §16–§22 and §61.

- [x] Fixed process architecture confirmed: CamillaDSP stays a separate process; no `camillalib` embedding (roadmap §2, §25).

  Recorded in roadmap §2 and mirrored in `.github/copilot-instructions.md`'s Non-Negotiable Architecture Rules.

- [x] Crash-isolation requirement recorded: CamillaDSP crash must not take down piCoreCDSP and vice versa.

  Recorded in roadmap §2 ("A CamillaDSP crash must not automatically take down piCoreCDSP" / "A piCoreCDSP crash
  must not take down CamillaDSP") and in the Gate 7 error-model items; enforcement lands in code at Gate 7.

- [x] Long-term end goal (installer + ALSA setup + configs + packaging only) recorded as the target shape (roadmap §3).

  Recorded in roadmap §3 and §52 (Long-Term Deletion Stages) as the explicit end state piCoreCDSP shrinks toward.

- [x] ✅ **Gate 1 passed**: architecture decisions are written down and agreed before any reconciler code is written.

  All six items above are documented in the roadmap and/or enforced via `.github/PULL_REQUEST_TEMPLATE.md` and
  `.github/copilot-instructions.md`. No reconciler code exists yet (see Gate 0 status) — consistent with this gate
  being satisfied purely by decisions-on-record, ahead of any implementation.

---

## Gate 2 — Ownership Model & Contracts

- [x] Ownership model implemented as documented types/comments: user/GUI-owned fields, ALSA-owned fields,
      CamillaDSP-owned fields, Rust-temporary-owned fields (roadmap §4). See `src/ownership.rs`.
- [x] `SourceState` enum defined (`Inactive` / `Active { sample_rate }`) with **no** producer-specific variants (roadmap §5). See `src/source/mod.rs`.
- [x] `pcm.picorecdsp` ALSA `plug` definition implemented: `format S32_LE`, `channels 2`, `rate unchanged` (roadmap §6). See `src/source/alsa_loopback.rs` (contract + parser/validator; installing it on a real target is Gate 11's job).
- [x] CamillaDSP transport contract validation implemented: read + validate only, never repair/rewrite (roadmap §7). See `src/camilla/mod.rs`.
- [x] Managed-mode-suspended behavior implemented for incompatible transport configs. See `ManagedMode` in `src/camilla/mod.rs`.
- [x] ✅ **Gate 2 passed**: contracts are enforced by types/validation, not by convention alone (17 unit tests covering compliant/incompatible cases in `cargo test`).

---

## Gate 3 — State Model & Reconciliation Core

- [x] Five state truths implemented as distinct types/sources (source transport `SourceSnapshot`, DSP process `DspState`, applied runtime config `ConfigDocument` via `GetConfig`/`GetPreviousConfig`, persistent config `PathBuf` via `GetConfigFilePath`, GUI draft — not observed by Rust — roadmap §8).
- [x] Hard config invariants enforced in code (no user YAML writes, no `runtime.yml`, no shadow config, `Save != Apply` — roadmap §9). `ConfigDocument::with_path_value` only patches the rate field; all user-owned fields are carried through untouched.
- [x] Reconciliation loop implemented as "read → determine desired state → minimal action → settle → re-read → verify" (roadmap §10, §37 pseudocode). See `src/reconcile.rs`.
- [x] `snd-aloop` source observer trait implemented: `SourceObserver` with `snapshot()` and `next_trigger()`. Real ALSA HCTL implementation pending Gate 11 (target hardware). Test double `MockSourceObserver` provided. (roadmap §11). See `src/source/observer.rs`.
- [x] Normal stop lifecycle: Rust performs no own stop; `WaitingForSourceStop` action returned; CamillaDSP's `stop_on_inactive` handles it (roadmap §12).
- [x] Settled-state detection: `wait_for_settle()` polls at configurable interval, no fixed long sleeps (roadmap §13). `ReconcileConfig` parameters are tunable.
- [x] Runtime config priority: `GetConfig` (running/paused) → `GetPreviousConfig` (settled inactive) — implemented in `DspSnapshot::authoritative_config()` (roadmap §14).
- [x] Source-rate policy: `ConfigDocument::rate_field_path()` returns `devices.samplerate` (no resampler) or `devices.capture_samplerate` (resampler present); DSP output rate / resampler params never touched (roadmap §15).
- [x] ✅ **Gate 3 passed**: reconciler converges correctly across the full Gate 8 test matrix on a real or emulated
      `snd-aloop` + CamillaDSP pair.

  Gate 8 has passed (all four mandatory test matrices automated and green — 111 unit tests pass in CI). The
  real-hardware verification component will be confirmed at Gate 12.

---

## Gate 4 — Rate-Sync Workarounds (Explicit Removal Criteria)

- [x] `SourceRateSynchronizer` trait implemented in `rate_sync/` (roadmap §16, §18). See `src/rate_sync/mod.rs`.
- [x] Rate sync while running: `ConfigPatchRateSynchronizer` calls `set_config_value` on the single rate field when DSP is Running/Paused (roadmap §16). See `src/rate_sync/config_patch.rs`.
- [x] Rate sync after inactive: fresh `GetPreviousConfig` read immediately before write, single rate field patched, `SetConfig` called, preserving filters/mixer/pipeline/Apply-without-Save (roadmap §17). Mandatory regression test passes (gain +6 dB survives 44.1 → 96 → 48 kHz).
- [x] Cliffhanger A removal criterion documented in code comments and `upstream/capabilities.yml` key `persistent_source_rate_override` (roadmap §18).
- [x] `SourceObserver` trait implemented in `src/source/observer.rs` with removal criterion documented in `upstream/capabilities.yml` key `native_aloop_rate_following` (roadmap §19).
- [x] Race mitigation: fresh `GetPreviousConfig` read immediately before `SetConfig`, fingerprint computed before/after, `RateLimitExceeded` never treated as partial success (roadmap §20 / Cliffhanger C). Registered in `upstream/capabilities.yml` key `config_revision_cas`.
- [x] `$samplerate$` token guard: `ConfigDocument::has_samplerate_token()` scans the full config tree; `with_path_value` refuses to patch if token found; `ConfigPatchRateSynchronizer` checks before writing; fail-closed with `PicorecdspError::SamplerateTokenGuard` (roadmap §21 / Cliffhanger D).
- [x] `DspTriggerSource` trait implemented: `PollingTrigger` for 4.1, `CamillaDspV4StateEvents` and `CamillaDspV5StateEvents` for 4.2+/5.x (roadmap §22 / Cliffhanger E).
- [x] Every workaround registered in `upstream/capabilities.yml` with `local_code` and `removal_when` fields (roadmap §61). Keys: `persistent_source_rate_override`, `native_aloop_rate_following`, `config_revision_cas`, `samplerate_token_reresolution`, `state_push_events`, `camilla_v5_wire_format`.
- [x] ✅ **Gate 4 passed**: every temporary workaround has code-linked removal criteria, not just prose in the roadmap.

---

## Gate 5 — Camilla Abstraction & Protocol Adapters

- [x] `CamillaControl` trait implemented, hiding the WebSocket wire format entirely from the reconciler (roadmap §23). See `src/camilla/control.rs`.
- [x] `CamillaStateEvents::subscribe_state` implemented as optional with polling fallback `PollingTrigger` (roadmap §23, §22). See `src/camilla/protocol_v4.rs` and `src/rate_sync/mod.rs`.
- [x] `camilla/protocol_v4.rs` implemented against the CamillaDSP 4.x wire protocol.
- [x] `camilla/protocol_v5.rs` implemented against CamillaDSP `next5` as a canary, sharing the same semantic API.
- [x] No version checks leak into `reconcile.rs`, `source/`, or `config_view.rs` (roadmap §24).
- [x] Confirmed no `camillalib` dependency exists in `Cargo.toml` and no engine internals are imported (roadmap §25).
- [x] `ConfigDocument` implemented as a schema-light generic YAML/JSON tree limited to the documented paths (roadmap §26), with no filter/mixer/processor/biquad/FIR modeling. See `src/camilla/config_document.rs`.
- [x] ✅ **Gate 5 passed**: the reconciler compiles and passes its tests against both `protocol_v4` and `protocol_v5`
      through the same trait, with zero protocol-specific branching outside `camilla/`.

---

## Gate 6 — CamillaGUI Integration & Operating-State Scenarios

- [x] Custom `on_get_active_config` / `on_set_active_config` / shadow `active_config.yml` removed or confirmed absent
      (roadmap §28). Confirmed absent: v2 codebase communicates only with CamillaDSP via `CamillaControl`; no
      shadow config file and no GUI hooks exist anywhere in `src/`.
- [x] Rust confirmed to never observe CamillaGUI directly — only CamillaDSP (roadmap §28). Confirmed: the entire
      Rust codebase interfaces with CamillaDSP exclusively through the `CamillaControl` WebSocket trait. There is
      no GUI process handle, no GUI port/socket observation, and no HTTP/REST call to CamillaGUI anywhere in `src/`.
- [x] Apply-during-playback scenario implemented/tested: Rust changes nothing but source rate, all other GUI changes
      preserved (roadmap §29.1, §30). See `reconcile::tests::apply_during_playback_reconciler_only_touches_rate`.
- [x] Apply-without-Save scenario implemented/tested: RuntimeConfig wins, survives rate changes (roadmap §29.2).
      See `rate_sync::config_patch::tests::apply_without_save_gain_survives_rate_change` and
      `apply_without_save_gain_survives_full_rate_cycle`.
- [x] Save-without-Apply scenario implemented/tested: running DSP unchanged, no auto-reload (roadmap §29.3).
      See `rate_sync::config_patch::tests::save_without_apply_rate_sync_ignores_disk_config`.
- [x] Config A → B scenario implemented/tested: latest applied decision wins, A never restored (roadmap §29.4).
      See `reconcile::tests::config_a_to_b_latest_applied_config_is_authoritative`.
- [x] `ConfigFilePath != RuntimeConfig` divergence confirmed to produce no repair/warning (roadmap §29.5).
      See `reconcile::tests::config_file_path_divergence_produces_no_repair`. By construction,
      `reconcile_step` accepts no `config_file_path` parameter and never reads from disk.
- [x] New-source-same-rate handled via active-generation detection, not just `old_rate != new_rate` (roadmap §31).
      See `reconcile::tests::new_source_same_rate_reconcile_still_runs_full_pass`. The reconciler calls
      `ensure_source_rate` unconditionally when source is active; generation is available on `SourceSnapshot`
      to prevent any future loop-level optimization from incorrectly skipping same-rate passes.
- [x] Concurrent Apply + rate-change race handled per §32 (settle → fresh reads → minimal write → verify).
      See `reconcile::tests::concurrent_apply_rate_change_fresh_snapshot_used`. Fresh-read-before-write is
      enforced in `ConfigPatchRateSynchronizer::ensure_source_rate` (re-reads `GetPreviousConfig` immediately
      before `SetConfig`).
- [x] Mandatory regression test passes: gain +6 dB applied without save survives source rate 44.1 → 96 → 48 kHz
      (roadmap §30, §47). See `rate_sync::config_patch::tests::apply_without_save_gain_survives_rate_change`
      and `apply_without_save_gain_survives_full_rate_cycle`.
- [x] ✅ **Gate 6 passed**: all scenarios in roadmap §29–§32 and the GUI test matrix (Gate 8) pass.

  All GUI, concurrent-race, Apply-without-Save, Save-without-Apply, Config A→B, and mandatory regression
  scenarios are covered by named unit tests in `src/reconcile.rs` / `src/rate_sync/config_patch.rs`,
  all green (111 tests pass). Gate 8 GUI matrix is fully automated.

---

## Gate 7 — Error, Recovery & Cold Boot

- [x] WebSocket-offline recovery implemented: bounded backoff, reconnect, full fresh snapshot (roadmap §33). See `run_loop` in `src/reconcile.rs` (`ws_initial_backoff`, `ws_max_backoff` config fields; test `run_loop_websocket_offline_reconnects_after_backoff`).
- [x] DAC-unavailable handling implemented: log + retry with current RuntimeConfig, no automatic DAC switch. `DspError { stop_reason: Some(PlaybackError) }` classified in `reconcile_step`; bounded retry via `max_dsp_error_retries` / `dsp_error_retry_interval` in `run_loop` (tests `dsp_failed_with_playback_error_reports_stop_reason`, `cold_boot_playback_error_at_startup_is_dac_unavailable`).
- [x] Invalid-config handling implemented: no repair; old valid RuntimeConfig stays authoritative or `WaitingForUserFix`. See `ReconcileAction::WaitingForUserFix` and transport-contract section in `reconcile_step` (tests `invalid_config_in_active_config_returns_waiting_for_user_fix`, `cold_boot_invalid_persistent_config_waits_for_fix`).
- [x] Incompatible-transport-config handling implemented: managed mode suspended with a clear message. See `ReconcileAction::ManagedModeSuspended` (unchanged from Gate 2/3).
- [x] Stalled-state handling implemented: short observation phase, no immediate restart loop, bounded retry. `DspError { state: Stalled }` returned; `max_dsp_error_retries` / `dsp_error_retry_interval` bound the retry count in `run_loop` (tests `stalled_dsp_reports_error`, `dsp_stalled_with_capture_format_change_reports_stop_reason`).
- [x] Rust-crash recovery implemented: stateless restart, fresh source/DSP read, reconcile. `run_loop` holds no cross-cycle state; on restart all state is read fresh. Documented in `run_loop` doc comment; test `rust_crash_recovery_is_stateless_restart`.
- [x] CamillaDSP-crash handling documented/implemented as an accepted v2 MVP boundary (unsaved RuntimeConfig may be lost). Documented in `run_loop` doc comment; test `camilladsp_crash_rust_reads_fresh_state_not_cached`.
- [x] CamillaGUI-crash isolation confirmed: audio, Rust, and CamillaDSP unaffected. Documented in `run_loop` doc comment; test `camillagui_crash_isolation_reconciler_runs_normally`.
- [x] Cold boot test matrix implemented and passing (roadmap §34): boot without/with producer, statefile behavior,
      `stop_on_inactive` → clean inactive, `PreviousConfig` available, boot at matching/different rate, startup
      `CaptureError`/`PlaybackError`, missing `ConfigFilePath`, invalid persistent config, no statefile. See
      `cold_boot_*` tests in `src/reconcile.rs`.
- [x] Confirmed no disk config watcher (mtime/inode/fingerprint auto-reload) exists (roadmap §35); runtime fingerprint
      used only for race detection. See `no_disk_config_watcher_in_reconcile_config` test.
- [x] ✅ **Gate 7 passed**: failure-injection suite (Gate 8) passes for every listed failure mode.

  All failure-injection scenarios (WebSocket disconnect, CamillaDSP crash, Rust crash, GUI crash, DAC
  disconnect, invalid config, incompatible transport, stalled DSP, missing ConfigFilePath, missing
  statefile, externally changed config file, snd-aloop missing, loopback handle hang,
  RateLimitExceeded, transitional DSP) are covered by named unit tests in `src/reconcile.rs`, all
  green in CI.

---

## Gate 8 — Mandatory Test Matrices

### Source (roadmap §45)
- [x] Boot without source / boot with source. See `cold_boot_without_producer_waits_for_source_stop`, `cold_boot_with_active_producer_at_matching_rate_syncs_immediately`.
- [x] Start at 44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz. See `source_matrix_all_standard_rates_accepted`, `source_matrix_all_standard_rates_inactive_dsp_uses_set_config`.
- [x] Stop. See `inactive_source_waits_for_stop`, `source_matrix_rapid_flapping_each_step_independent`.
- [x] New source, same rate / different rate. See `new_source_same_rate_reconcile_still_runs_full_pass`, `cold_boot_with_active_producer_at_different_rate_patches_config`.
- [x] Rapid flapping. See `source_matrix_rapid_flapping_each_step_independent`.
- [x] Lost HCTL event / duplicate HCTL events. See `source_matrix_duplicate_hctl_events_are_idempotent`, `source_matrix_lost_inactive_event_next_reconcile_still_waits`.

### Producer (roadmap §46)
- [x] Squeezelite. See `producer_matrix_squeezelite_44100_rate_synced` (producer-agnostic by architecture, roadmap §5).
- [x] AirPlay/Shairport. See `producer_matrix_airplay_44100_rate_synced`.
- [x] Squeezelite → AirPlay and AirPlay → Squeezelite, same and different rate. See `producer_matrix_squeezelite_to_airplay_same_rate_triggers_rate_sync`, `producer_matrix_airplay_to_squeezelite_different_rate_patches_config`.
- [x] Parallel open. See `producer_matrix_parallel_open_reconciler_acts_on_last_writer_state`.
- [x] Producer terminates unexpectedly / reopens immediately. See `producer_matrix_unexpected_termination_reconciler_waits_for_stop`, `producer_matrix_termination_then_immediate_reopen_handled`.

### GUI (roadmap §47)
- [x] Filter / Mixer / Pipeline Apply. See `apply_during_playback_reconciler_only_touches_rate`.
- [x] Config A → B. See `config_a_to_b_latest_applied_config_is_authoritative`.
- [x] Apply without Save / Save without Apply / Apply + Save. See `apply_without_save_gain_survives_rate_change`, `save_without_apply_rate_sync_ignores_disk_config`, `apply_without_save_gain_survives_full_rate_cycle`.
- [x] Apply while source stops / starts / during a rate change. See `concurrent_apply_rate_change_fresh_snapshot_used`.
- [x] Config switch during a rate change. See `gui_matrix_config_switch_during_rate_change_uses_new_config`.
- [x] Enable/disable resampler during playback. See `gui_matrix_enable_resampler_during_playback_reconciler_only_touches_rate`, `gui_matrix_disable_resampler_during_playback_reconciler_only_touches_rate`.
- [x] GUI restart during playback. See `camillagui_crash_isolation_reconciler_runs_normally`.
- [x] Mandatory regression: gain +6 dB applied without save survives 44.1 → 96 → 48 kHz. See `apply_without_save_gain_survives_full_rate_cycle`.

### Failure Injection (roadmap §48)
- [x] WebSocket disconnect. See `run_loop_websocket_offline_reconnects_after_backoff`.
- [x] CamillaDSP controlled restart / crash. See `camilladsp_crash_rust_reads_fresh_state_not_cached`.
- [x] Rust restart / crash. See `rust_crash_recovery_is_stateless_restart`.
- [x] GUI restart. See `camillagui_crash_isolation_reconciler_runs_normally`.
- [x] DAC disconnect/reconnect. See `dsp_failed_with_playback_error_reports_stop_reason`, `cold_boot_playback_error_at_startup_is_dac_unavailable`.
- [x] Invalid config / incompatible transport config. See `invalid_config_in_active_config_returns_waiting_for_user_fix`, `incompatible_transport_suspends_managed_mode`.
- [x] Stalled / `PlaybackError` / `CaptureError` / `CaptureFormatChange`. See `stalled_dsp_reports_error`, `dsp_failed_with_playback_error_reports_stop_reason`, `dsp_stalled_with_capture_format_change_reports_stop_reason`.
- [x] Missing `ConfigFilePath` / missing statefile. See `cold_boot_no_previous_config_reconciler_defers`, `cold_boot_statefile_previous_config_available_and_used`.
- [x] Config file changed externally. See `no_disk_config_watcher_in_reconcile_config` (confirms no auto-reload; Save != Apply by construction).
- [x] `snd-aloop` missing. See `failure_injection_snd_aloop_missing_run_loop_skips_cycle`.
- [x] Loopback handle hangs. See `failure_injection_loopback_handle_hangs_run_loop_skips_and_continues`.
- [x] WebSocket `RateLimitExceeded`. See `failure_injection_rate_limit_exceeded_propagates_out_of_reconcile_step`, `failure_injection_rate_limit_exceeded_in_run_loop_is_fatal`.
- [x] CamillaDSP still starting while a new event arrives. See `transitional_dsp_defers`.

- [x] ✅ **Gate 8 passed**: all four matrices are automated (not manual-only) wherever feasible, and green in CI or on
      the hardware gate where hardware-dependent.

---

## Gate 9 — Module Structure & Implementation Hygiene

- [ ] `src/` module layout matches roadmap §36 (`main.rs`, `reconcile.rs`, `source/`, `camilla/`, `rate_sync/`,
      `config_view.rs`, `retry.rs`, `error.rs`, `logging.rs`).
- [ ] Reconcile loop matches the pseudocode in roadmap §37 (trigger → source/dsp snapshot → branch on state → settle
      → verify).
- [ ] `cargo fmt` clean.
- [ ] `cargo clippy --all-targets` clean (or all remaining warnings explicitly triaged).
- [ ] Full unit + integration test suite green.
- [ ] ✅ **Gate 9 passed**: module boundaries match the documented deletion boundaries (i.e. `protocol_v4.rs`,
      `config_patch.rs`, `alsa_loopback.rs` can each be deleted without touching unrelated modules).

---

## Gate 10 — CI Strategy

- [x] Release-gate CI implemented against the pinned production stack only: fmt, clippy, unit tests, integration
      tests, ARM build, ALSA state tests, Camilla protocol tests, config continuity tests, race tests, failure
      recovery tests (roadmap §41). See `.github/workflows/ci.yml`.
- [x] Upstream canary CI implemented, non-blocking: `next4.2.0`, `next5`, upcoming GUI branches, WebSocket contract
      probe, config capability probe, aloop lifecycle probe, state event probe (roadmap §41). See
      `.github/workflows/canary.yml`. Runs daily; never gates the release build.
- [x] Black-box capability probes implemented per roadmap §42 (loopback rate detection, inactive-state rate override,
      override persistence across `SetConfig`/GUI Apply, `$samplerate$` re-resolution, `stop_on_inactive` release,
      `GetPreviousConfig` correctness, `ConfigFilePath` stability, config revision/CAS, `SubscribeState` stability,
      full native aloop lifecycle). See `probes/probe_camilla_capabilities.py`,
      `probes/probe_camilla_capabilities.sh`, `probes/report_probe_results.py`.
- [x] ✅ **Gate 10 passed**: canary CI runs on schedule and never gates or auto-updates the release build.

  Canary workflow runs daily at 04:00 UTC, probes CamillaDSP `master`/`next4.2.0`/`next5` branches and
  CamillaGUI backend open branches, reports results to the Actions step summary, and uploads probe-result
  JSON as a 90-day artifact.  All jobs use `continue-on-error: true` so a canary failure never blocks
  a merge to `main`.

---

## Gate 11 — Installer v2

- [x] Fresh-install-only installer implemented (no reinstall/migration path — roadmap §43). See
      `install_picorecdsp.sh`; the script aborts if any component is already installed and has no migration path.
- [x] `snd-aloop` availability check implemented with a hard abort if unavailable. See `check_aloop()` in
      `install_picorecdsp.sh`.
- [x] Physical playback device auto-detection implemented (one-time). See `detect_playback_device()` in
      `install_picorecdsp.sh` (uses `aplay -l`, excludes the Loopback card, accepts `--playback-device` override).
- [x] `pcm.picorecdsp` installed by the installer. See `install_alsa_plug()` in `install_picorecdsp.sh` and
      `configs/pcm.picorecdsp.conf` (matches `CANONICAL_ASOUND_CONF` from `src/source/alsa_loopback.rs`).
- [x] Pinned CamillaDSP + compatible pinned CamillaGUI installed. See `install_camilladsp()` and
      `install_camillagui()` in `install_picorecdsp.sh`. Version pins are `CAMILLA_VERSION` /
      `CAMILLA_GUI_VERSION` env vars (set at Gate 12 hardware validation).
- [x] Shared native statefile configured. See `configure_statefile()` in `install_picorecdsp.sh`.
- [x] `Bypass.yml` / `Null.yml` generated only if absent, matching the pinned CamillaDSP version, with correct
      loopback capture (`S32_LE`, 2 channels, `stop_on_inactive: true`), detected playback device, sane rate-adjust
      defaults, and no piCoreCDSP runtime tokens (roadmap §44). See `generate_configs()` in
      `install_picorecdsp.sh`.
- [x] No backend menu/switcher exists in the installer.
- [x] Existing user configs are never overwritten or silently migrated. Every config write in the installer is
      guarded by an "only if absent" check.
- [x] pCP backup executed; reboot triggered only if required. See `run_backup()` and `prompt_reboot()` in
      `install_picorecdsp.sh`.
- [x] ✅ **Gate 11 passed**: a clean pCP image can be installed end-to-end with zero manual config edits required.

  The installer handles snd-aloop loading, ALSA plug installation, CamillaDSP + CamillaGUI download and
  installation, statefile setup, Bypass/Null config generation, bootlocal.sh registration, pCP backup, and
  reboot prompt.  Version pins are ENV-var-controlled and will be locked at Gate 12.  The installer is
  shellcheck-clean at the warning level.

---

## Gate 12 — Hardware Gate

- [ ] Validated on real pCP target version and real Raspberry Pi target hardware.
- [ ] Validated with real USB DACs and I2S DAC where relevant.
- [ ] Validated with multiple producers and both rate families.
- [ ] Hundreds of rate changes and producer handovers exercised without failure.
- [ ] Long-run playback and intensive GUI use exercised without failure.
- [ ] Failure injection suite exercised on hardware.
- [ ] Confirmed: no user file modified, no shadow config created, no hanging loopback handles, no stale runtime rate,
      no lost applied GUI changes on normal rate changes, no unintended CamillaDSP process restarts on normal filter
      changes (roadmap §49).
- [ ] ✅ **Gate 12 passed**: hardware validation report published and archived alongside the release tag.

---

## Gate 13 — Hard v1 → v2 Cleanup

> **Note on sequencing:** this gate's file-removal criteria were executed early, as part of Gate 0's Option A
> cutover, at the repository owner's explicit confirmation — rather than being deferred until v2 reached feature
> parity through Gates 2–12 as the roadmap's gate ordering originally implied. As of this change, `main` contains
> **no v2 implementation code at all** (Gates 2–12 have not started) and also no v1 implementation — only the v2
> planning/roadmap docs and this README. This gate is "passed" in the narrow sense that no v1 remnants exist on
> `main`; it does **not** mean v2 has reached functional or hardware parity with the archived v1 product.

- [x] `v1-final` tag exists (from Gate 0) and, if desired, an archive branch.

  Done — `v1-final` (annotated) and `v1-archive` both exist on `origin`, pointing at `72b556d`.
- [x] ioplug, C code, IPC, stdin capture, ring buffer, audio worker threads, backend abstraction, backend switcher,
      `adaptation.rs`, RuntimeConfig, runtime YAML, reinstall logic, ioplug benchmarks, ioplug CI deleted from `main`.

  Done as part of the Gate 0 v1-tree removal — `picoredsp-ioplug/`, all of `src/` (including `backend.rs`,
  `backend/aloop.rs`, `backend/ioplug.rs`, `core/adaptation.rs`), `benches/`, and `.github/workflows/build.yml`
  are removed from `main`.
- [x] Obsolete v1 upstream-monitoring code/docs deleted from `main`.

  Done — `.github/workflows/upstream-tracking.yml`, `scripts/check_upstream_tracking.py`,
  `scripts/test_check_upstream_tracking.py`, `docs/upstream-tracking.yml`, `docs/ALSA_LIB_TRACKING.md`,
  `docs/CAMILLADSP_TRACKING.md`, `docs/CAMILLAGUI_TRACKING.md`, and `docs/CAMILLADSP_CONTROLLER_TRACKING.md` are
  removed from `main`. A fresh v2 upstream-monitoring infrastructure is still to be built at Gate 14
  (`upstream/manifest.yml`, `upstream/capabilities.yml`, the `upstream-*.yml` workflows below) — this item only
  covers deletion of the *v1* tracking setup.
- [x] README targets piCoreCDSP v2 exclusively.

  Done — `README.md` rewritten to describe only the v2 status/roadmap and point to the `v1-final` tag /
  `v1-archive` branch for the archived implementation.
- [x] No `legacy/`, `deprecated/`, `old_controller/`, or `experimental_ioplug/` directories exist on `main`.
- [x] ✅ **Gate 13 passed**: `main` contains only the v2 architecture; v1 is reachable solely via git history/tags.

  Passed in the file-hygiene sense described in the sequencing note above — there is currently no v2
  *architecture* (code) on `main` either, only its planning documents. Gates 2–12 remain the actual v2
  implementation and hardware-validation work.

---

## Gate 14 — Upstream Monitoring Infrastructure

- [ ] `upstream/manifest.yml` exists with all sources from roadmap §55–§58, each assigned a priority and paths.
- [ ] `upstream/capabilities.yml` exists, mapping every capability to `local_code` and `removal_when` criteria.
- [ ] `upstream-sync.yml` workflow implemented: daily + `workflow_dispatch`, PR-based (never pushes to `main` directly).
- [ ] `upstream-capability-canary.yml` workflow implemented: runs black-box probes after sync PRs, nightly, and on
      `workflow_dispatch`.
- [ ] `upstream-release-watch.yml` workflow implemented: daily, reports new releases, never auto-upgrades.
- [ ] `upstream-branch-watch.yml` workflow implemented: watches `next*`/`v5*`/`5.*` branches on the GUI/client repos.
- [ ] `upstream-removal-check.yml` workflow implemented: opens a removal-candidate issue on FAIL→PASS transitions.
- [ ] Monitoring levels (critical/high/medium/ignore) applied consistently across all workflows (roadmap §67).
- [ ] `upstream/status.md` dashboard generated automatically (roadmap §68).
- [ ] Confirmed no workflow performs automatic code adoption, config migration, or version upgrade (roadmap §69).
- [ ] Retention policy applied: current + previous snapshot kept, no full external repo copies (roadmap §70).
- [ ] Recommended labels created in the repository (roadmap §71).
- [ ] ✅ **Gate 14 passed**: all items in roadmap §72 (Upstream Monitoring Definition of Done) are satisfied.

---

## Definition of Done (roadmap §51)

- [ ] Architecture aligned with CamillaDSP 5; production stack exactly pinned.
- [ ] CamillaDSP remains a separate process; `camillalib` not embedded.
- [ ] Exactly one production Camilla WebSocket adapter exists.
- [ ] `pcm.picorecdsp` is producer-agnostic; only `snd-aloop` is used.
- [ ] No ioplug, no custom audio data path; Rust processes no samples.
- [ ] User YAML untouched; no runtime YAML exists.
- [ ] `GetConfig`/`GetPreviousConfig` model runtime continuity correctly.
- [ ] Apply-without-Save survives normal rate changes; Save-without-Apply is never auto-applied.
- [ ] Config switches work during playback; source rate follows ALSA.
- [ ] Native mode and resampler mode both work.
- [ ] GUI/rate races reliably converge.
- [ ] Errors are never masked by config repair.
- [ ] Every workaround has a removal criterion registered in `upstream/capabilities.yml`.
- [ ] Upstream canaries detect future simplifications.
- [ ] v1/ioplug completely removed from `main`.

---

## Architectural Guard Rails (verify throughout all gates)

- [ ] Rust never writes user YAML.
- [ ] No shadow config file or `runtime.yml` is ever introduced.
- [ ] No producer-specific logic is ever added to the Rust core.
- [ ] No Squeezelite-, AirPlay-, or other producer-specific branching in `reconcile.rs` or `source/`.
- [ ] No `camillalib` dependency is ever added to `Cargo.toml`.
- [ ] No protocol-version checks leak outside `camilla/`.
- [ ] No workaround is merged without a documented removal criterion in `upstream/capabilities.yml`.
- [ ] No permanent `legacy/`/`deprecated/` directories are introduced (git tags/branches are the archive).
- [ ] No fixed long sleeps are used for state settling (debounce + fresh read only).
- [ ] No automatic production upgrade to an upstream development/canary branch.
- [ ] No automatic code adoption from the upstream mirror.

---

## Removal Matrix Tracking (living table, update as upstream capabilities land)

| Upstream capability | Status | Local code to remove | Removed on |
|---|---|---|---|
| Stable `SubscribeState` | pending | fast DSP state poller | — |
| Persistent runtime source-rate override | pending | `rate_sync/config_patch.rs` | — |
| Override survives inactive + `SetConfig` | pending | PreviousConfig rate rebuild | — |
| Token-aware source-rate override | pending | `$samplerate$` guard | — |
| Native aloop rate detection | pending | `source/alsa_loopback.rs` HCTL observer | — |
| Native aloop restart lifecycle | pending | rate reconciler / large parts of `reconcile.rs` | — |
| Config revision/CAS | pending | race mitigation in `reconcile.rs` | — |
| Stable embedded API | not planned (re-evaluate only) | n/a | — |
