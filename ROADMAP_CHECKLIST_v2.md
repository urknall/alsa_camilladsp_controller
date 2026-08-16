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

- [ ] Ownership model implemented as documented types/comments: user/GUI-owned fields, ALSA-owned fields,
      CamillaDSP-owned fields, Rust-temporary-owned fields (roadmap §4).
- [ ] `SourceState` enum defined (`Inactive` / `Active { sample_rate }`) with **no** producer-specific variants (roadmap §5).
- [ ] `pcm.picorecdsp` ALSA `plug` definition implemented: `format S32_LE`, `channels 2`, `rate unchanged` (roadmap §6).
- [ ] CamillaDSP transport contract validation implemented: read + validate only, never repair/rewrite (roadmap §7).
- [ ] Managed-mode-suspended behavior implemented for incompatible transport configs.
- [ ] ✅ **Gate 2 passed**: contracts are enforced by types/validation, not by convention alone.

---

## Gate 3 — State Model & Reconciliation Core

- [ ] Five state truths implemented as distinct types/sources (source transport, DSP process, applied runtime config,
      persistent config, GUI draft — roadmap §8).
- [ ] Hard config invariants enforced in code (no user YAML writes, no `runtime.yml`, no shadow config, `Save != Apply` — roadmap §9).
- [ ] Reconciliation loop implemented as "read → determine desired state → minimal action → settle → re-read → verify"
      (roadmap §10, §37 pseudocode), not as a historical/imperative state machine.
- [ ] `snd-aloop` source observer implemented: non-blocking HCTL, event subscribe, debounce (~50 ms tested), fresh
      snapshot re-read, slow periodic safety snapshot (roadmap §11).
- [ ] Normal stop lifecycle implemented: no Rust-initiated stop in the normal case, grace/settle phase, safety stop
      only on unexpected hang (roadmap §12).
- [ ] Settled-state detection implemented (debounce + fresh read + re-check, no fixed long sleeps — roadmap §13).
- [ ] Runtime config priority implemented: `GetConfig` (running/paused) → `GetPreviousConfig` (settled inactive) →
      statefile/`ConfigFilePath` (cold boot) (roadmap §14).
- [ ] Source-rate policy implemented for both the no-resampler and resampler cases, without ever touching resampler
      type/quality, DSP output rate, chunksize, or target level (roadmap §15).
- [ ] ✅ **Gate 3 passed**: reconciler converges correctly across the full Gate 8 test matrix on a real or emulated
      `snd-aloop` + CamillaDSP pair.

---

## Gate 4 — Rate-Sync Workarounds (Explicit Removal Criteria)

- [ ] `SourceRateSynchronizer` trait implemented in `rate_sync/` (roadmap §16, §18).
- [ ] Rate sync while running implemented: fresh `GetConfig`, transport check, single-field rate write, settle, verify (roadmap §16).
- [ ] Rate sync after inactive implemented: fresh `GetPreviousConfig`, transport check, single-field rate write,
      `SetConfig`, settle, verify — preserving filters/mixer/pipeline/Apply-without-Save (roadmap §17).
- [ ] Cliffhanger A removal criterion documented in code comments/`capabilities.yml` linking to the trait (roadmap §18).
- [ ] `SourceObserver` trait implemented in `source/alsa_loopback.rs` with its removal criterion documented (roadmap §19).
- [ ] Race mitigation implemented per Cliffhanger C: fresh reads immediately before writes, no long-lived config
      caching, runtime fingerprinting, discard-and-reconcile on mismatch (roadmap §20).
- [ ] `$samplerate$` token guard implemented: known cases detected, fail-closed with a clear message, no custom
      template engine (roadmap §21).
- [ ] `DspTriggerSource` trait implemented, defaulting to polling on 4.1 and switching to `SubscribeState` +
      slow safety reconcile on 4.2/5 (roadmap §22).
- [ ] Every workaround above is registered in `upstream/capabilities.yml` with `local_code` and `removal_when` fields
      (roadmap §61).
- [ ] ✅ **Gate 4 passed**: every temporary workaround has code-linked removal criteria, not just prose in the roadmap.

---

## Gate 5 — Camilla Abstraction & Protocol Adapters

- [ ] `CamillaControl` trait implemented, hiding the WebSocket wire format entirely from the reconciler (roadmap §23).
- [ ] `CamillaStateEvents::subscribe_state` implemented as optional, with a polling fallback (roadmap §23, §22).
- [ ] `camilla/protocol_v4.rs` implemented against the pinned CamillaDSP 4.x line.
- [ ] `camilla/protocol_v5.rs` implemented against CamillaDSP `next5` as a canary, sharing the same semantic API.
- [ ] No version checks leak into `reconcile.rs`, `source/`, or `config_view.rs` (roadmap §24).
- [ ] Confirmed no `camillalib` dependency exists in `Cargo.toml` and no engine internals are imported (roadmap §25).
- [ ] `ConfigDocument` implemented as a schema-light generic YAML/JSON tree limited to the documented paths
      (roadmap §26), with no filter/mixer/processor/biquad/FIR modeling.
- [ ] ✅ **Gate 5 passed**: the reconciler compiles and passes its tests against both `protocol_v4` and `protocol_v5`
      through the same trait, with zero protocol-specific branching outside `camilla/`.

---

## Gate 6 — CamillaGUI Integration & Operating-State Scenarios

- [ ] Custom `on_get_active_config` / `on_set_active_config` / shadow `active_config.yml` removed or confirmed absent
      (roadmap §28).
- [ ] Rust confirmed to never observe CamillaGUI directly — only CamillaDSP (roadmap §28).
- [ ] Apply-during-playback scenario implemented/tested: Rust changes nothing but source rate, all other GUI changes
      preserved (roadmap §29.1, §30).
- [ ] Apply-without-Save scenario implemented/tested: RuntimeConfig wins, survives rate changes (roadmap §29.2).
- [ ] Save-without-Apply scenario implemented/tested: running DSP unchanged, no auto-reload (roadmap §29.3).
- [ ] Config A → B scenario implemented/tested: latest applied decision wins, A never restored (roadmap §29.4).
- [ ] `ConfigFilePath != RuntimeConfig` divergence confirmed to produce no repair/warning (roadmap §29.5).
- [ ] New-source-same-rate handled via active-generation detection, not just `old_rate != new_rate` (roadmap §31).
- [ ] Concurrent Apply + rate-change race handled per §32 (settle → fresh reads → minimal write → verify).
- [ ] Mandatory regression test passes: gain +6 dB applied without save survives source rate 44.1 → 96 → 48 kHz
      (roadmap §30, §47).
- [ ] ✅ **Gate 6 passed**: all scenarios in roadmap §29–§32 and the GUI test matrix (Gate 8) pass.

---

## Gate 7 — Error, Recovery & Cold Boot

- [ ] WebSocket-offline recovery implemented: bounded backoff, reconnect, full fresh snapshot (roadmap §33).
- [ ] DAC-unavailable handling implemented: log + retry with current RuntimeConfig, no automatic DAC switch.
- [ ] Invalid-config handling implemented: no repair; old valid RuntimeConfig stays authoritative or `WaitingForUserFix`.
- [ ] Incompatible-transport-config handling implemented: managed mode suspended with a clear message.
- [ ] Stalled-state handling implemented: short observation phase, no immediate restart loop, bounded retry.
- [ ] Rust-crash recovery implemented: stateless restart, fresh source/DSP read, reconcile.
- [ ] CamillaDSP-crash handling documented/implemented as an accepted v2 MVP boundary (unsaved RuntimeConfig may be lost).
- [ ] CamillaGUI-crash isolation confirmed: audio, Rust, and CamillaDSP unaffected.
- [ ] Cold boot test matrix implemented and passing (roadmap §34): boot without/with producer, statefile behavior,
      `stop_on_inactive` → clean inactive, `PreviousConfig` available, boot at matching/different rate, startup
      `CaptureError`/`PlaybackError`, missing `ConfigFilePath`, invalid persistent config, no statefile.
- [ ] Confirmed no disk config watcher (mtime/inode/fingerprint auto-reload) exists (roadmap §35); runtime fingerprint
      used only for race detection.
- [ ] ✅ **Gate 7 passed**: failure-injection suite (Gate 8) passes for every listed failure mode.

---

## Gate 8 — Mandatory Test Matrices

### Source (roadmap §45)
- [ ] Boot without source / boot with source.
- [ ] Start at 44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz.
- [ ] Stop.
- [ ] New source, same rate / different rate.
- [ ] Rapid flapping.
- [ ] Lost HCTL event / duplicate HCTL events.

### Producer (roadmap §46)
- [ ] Squeezelite.
- [ ] AirPlay/Shairport.
- [ ] Squeezelite → AirPlay and AirPlay → Squeezelite, same and different rate.
- [ ] Parallel open.
- [ ] Producer terminates unexpectedly / reopens immediately.

### GUI (roadmap §47)
- [ ] Filter / Mixer / Pipeline Apply.
- [ ] Config A → B.
- [ ] Apply without Save / Save without Apply / Apply + Save.
- [ ] Apply while source stops / starts / during a rate change.
- [ ] Config switch during a rate change.
- [ ] Enable/disable resampler during playback.
- [ ] GUI restart during playback.
- [ ] Mandatory regression: gain +6 dB applied without save survives 44.1 → 96 → 48 kHz.

### Failure Injection (roadmap §48)
- [ ] WebSocket disconnect.
- [ ] CamillaDSP controlled restart / crash.
- [ ] Rust restart / crash.
- [ ] GUI restart.
- [ ] DAC disconnect/reconnect.
- [ ] Invalid config / incompatible transport config.
- [ ] Stalled / `PlaybackError` / `CaptureError` / `CaptureFormatChange`.
- [ ] Missing `ConfigFilePath` / missing statefile.
- [ ] Config file changed externally.
- [ ] `snd-aloop` missing.
- [ ] Loopback handle hangs.
- [ ] WebSocket `RateLimitExceeded`.
- [ ] CamillaDSP still starting while a new event arrives.

- [ ] ✅ **Gate 8 passed**: all four matrices are automated (not manual-only) wherever feasible, and green in CI or on
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

- [ ] Release-gate CI implemented against the pinned production stack only: fmt, clippy, unit tests, integration
      tests, ARM build, ALSA state tests, Camilla protocol tests, config continuity tests, race tests, failure
      recovery tests (roadmap §41).
- [ ] Upstream canary CI implemented, non-blocking: `next4.2.0`, `next5`, upcoming GUI branches, WebSocket contract
      probe, config capability probe, aloop lifecycle probe, state event probe (roadmap §41).
- [ ] Black-box capability probes implemented per roadmap §42 (loopback rate detection, inactive-state rate override,
      override persistence across `SetConfig`/GUI Apply, `$samplerate$` re-resolution, `stop_on_inactive` release,
      `GetPreviousConfig` correctness, `ConfigFilePath` stability, config revision/CAS, `SubscribeState` stability,
      full native aloop lifecycle).
- [ ] ✅ **Gate 10 passed**: canary CI runs on schedule and never gates or auto-updates the release build.

---

## Gate 11 — Installer v2

- [ ] Fresh-install-only installer implemented (no reinstall/migration path — roadmap §43).
- [ ] `snd-aloop` availability check implemented with a hard abort if unavailable.
- [ ] Physical playback device auto-detection implemented (one-time).
- [ ] `pcm.picorecdsp` installed by the installer.
- [ ] Pinned CamillaDSP + compatible pinned CamillaGUI installed.
- [ ] Shared native statefile configured.
- [ ] `Bypass.yml` / `Null.yml` generated only if absent, matching the pinned CamillaDSP version, with correct
      loopback capture (`S32_LE`, 2 channels, `stop_on_inactive: true`), detected playback device, sane rate-adjust
      defaults, and no piCoreCDSP runtime tokens (roadmap §44).
- [ ] No backend menu/switcher exists in the installer.
- [ ] Existing user configs are never overwritten or silently migrated.
- [ ] pCP backup executed; reboot triggered only if required.
- [ ] ✅ **Gate 11 passed**: a clean pCP image can be installed end-to-end with zero manual config edits required.

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
