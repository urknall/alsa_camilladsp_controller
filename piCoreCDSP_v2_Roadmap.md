# piCoreCDSP v2 — CamillaDSP-Native Architecture Roadmap

**Status:** Active (v2) — this is the current source of truth. It supersedes `piCoreDSP_Dual_Backend_Roadmap.md` (v1,
dual-backend `aloop`/`ioplug` architecture). Per Gate 0 of `ROADMAP_CHECKLIST_v2.md`, that file and the v1 source
tree have been removed from `main`'s working copy; they remain reachable via the `v1-final` tag / `v1-archive`
branch.

**Source plan:** [`docs/new plan/piCoreCDSP_v2_complete_roadmap(1).md`](docs/new%20plan/piCoreCDSP_v2_complete_roadmap%281%29.md) — the uploaded plan this roadmap formalizes. That file is retained verbatim as the original submission; this document is the curated, repository-canonical roadmap derived from it.

**Companion file:** [`ROADMAP_CHECKLIST_v2.md`](ROADMAP_CHECKLIST_v2.md) — actionable, gated checklist tracking progress on this roadmap.

**Strategic design target:** CamillaDSP 5
**Interim/test platform:** CamillaDSP 4.2
**Currently stable reference:** CamillaDSP 4.1.3 / CamillaGUI 4.1.0
**Fixed architecture decision:** CamillaDSP always remains a separate process. `camillalib` is never embedded.

> **This roadmap replaces the v1 dual-backend (`snd-aloop` + custom `ioplug`) architecture entirely.** piCoreCDSP v2 is not an evolution of the v1 controller/ioplug codebase. It is a fresh, deliberately small core built directly against CamillaDSP's own process/WebSocket/ALSA model.

---

## 0. Repository Reset — Legacy Isolation Strategy

Before v2 implementation work begins, the repository must be put into a state where the v1 codebase, v1 roadmap/checklist, and v1-era documentation cannot silently influence v2 work — including across agent restarts that re-read repository files for context.

Two isolation strategies were considered:

**Option A — Git-archive-only reset (recommended, and consistent with §50/§53 below):**
- [ ] Tag the current `main` HEAD as `v1-final` before any v2 work lands.
- [ ] Optionally cut an `v1-archive` branch from that tag for easy diffing/checkout.
- [ ] Remove the v1 source tree (`src/`, `picoredsp-ioplug/`, `benches/`, `scripts/`, `install_picoredsp.sh`, `CONFIG_MIGRATION.md`, v1 CI workflows) from `main` once v2 work is ready to start, relying on git history/tags as the sole archive.
- [ ] This matches the plan's own explicit policy in §50/§53: no permanent `legacy/`, `deprecated/`, `old_controller/`, or `experimental_ioplug/` directories — **git is the archive**.

**Option B — Reference subfolder (what was proposed in the request):**
- [ ] Move the entire current `src/`, `picoredsp-ioplug/`, `benches/`, `scripts/`, `install_picoredsp.sh`, and v1 docs into a clearly named, non-buildable reference folder (e.g. `reference/v1-legacy/`), excluded from the Cargo workspace, CI, and Copilot's default working set.
- [ ] Trade-off: keeps a working copy available without checking out a tag, at the cost of introducing exactly the kind of permanent legacy directory the new plan explicitly rejects (§50) — this option should be treated as a temporary transitional aid, not a permanent structure, and cleaned up at the same v1→v2 cleanup gate described in §50.

**Decision required before implementation starts:** pick Option A or Option B (or a time-boxed B → A transition) and record it as the first item of Gate 0 in the checklist. Until that decision is made and executed, no v2 source code should be added to `src/` — new v2 code must not be interleaved with v1 code in the same modules.

Regardless of which option is chosen, the following apply immediately (documentation-only, no code changes):
- [ ] `.github/copilot-instructions.md` now points to this roadmap and `ROADMAP_CHECKLIST_v2.md` as the sole source of truth for new work (done as part of this change).
- [ ] `piCoreDSP_Dual_Backend_Roadmap.md` and `ROADMAP_CHECKLIST.md` are marked superseded/frozen and must not be used to plan new v2 work; they remain only as historical record of the v1 effort until the Option A/B reset above is executed.
- [ ] Any future session must re-read this roadmap and its checklist before writing v2 code, exactly as the v1 instructions required for the v1 files.

---

## 1. Strategic Guiding Decision

piCoreCDSP v2 is **not** built as a continuation of the old controller/ioplug architecture. Instead:

- [ ] v2 is built as a fresh, small core.
- [ ] CamillaDSP 5 is the semantic design target.
- [ ] CamillaDSP 4.2 serves as the interim/test platform.
- [ ] CamillaDSP 4.1.3 serves only as a stable reference baseline.
- [ ] Exactly one CamillaDSP/CamillaGUI combination is pinned as the production stack before the first v2 product release.
- [ ] If CamillaDSP 5 plus a matching, hardware-validated CamillaGUI/pyCamillaDSP stack is stable in time, v2 ships directly on 5.
- [ ] Otherwise, v2 may ship initially on 4.2.
- [ ] No permanent multi-version compatibility is maintained.
- [ ] Development adapters for old versions are deleted after an upgrade.
- [ ] Git history and tags are the archive.
- [ ] Local workarounds are built exclusively as temporary compatibility bridges.
- [ ] Every workaround has an explicit removal criterion.
- [ ] When CamillaDSP upstream absorbs a capability, the corresponding local code is deleted, not kept running in parallel indefinitely.

**Guiding rule:**

> **Design for CamillaDSP 5 semantics, ship only against a pinned and hardware-validated release stack.**

---

## 2. Fixed Process Architecture

CamillaDSP remains permanently a separate process.

```text
Squeezelite / AirPlay / other ALSA producers
                  │
                  ▼
            pcm.camilladsp
                  │
                  ▼
              snd-aloop
                  │
                  ▼
        ┌──────────────────┐
        │   CamillaDSP     │
        │ separate process │
        └────────┬─────────┘
                 ▲
                 │ WebSocket
          ┌──────┴──────┐
          │             │
      piCoreCDSP     CamillaGUI
         (Rust)
```

Fixed decisions:

- [ ] `camillalib` is not embedded into piCoreCDSP.
- [ ] piCoreCDSP owns no CamillaDSP engine code.
- [ ] piCoreCDSP never uses CamillaDSP's internal `SharedConfigs`, `ControllerMessage`, `StatusStructs`, or other engine internals.
- [ ] CamillaDSP remains independently startable/restartable/upgradable.
- [ ] CamillaGUI and piCoreCDSP talk to the same public CamillaDSP control plane.
- [ ] WebSocket remains the integration boundary.
- [ ] ALSA remains the audio/source-state boundary.
- [ ] A CamillaDSP crash must not automatically take down piCoreCDSP.
- [ ] A piCoreCDSP crash must not take down CamillaDSP.
- [ ] CamillaDSP upgrades should, wherever possible, remain possible without rebuilding the piCoreCDSP core.
- [ ] Future library-level integration is only re-evaluated if upstream explicitly guarantees a stable first-class embedded API.

Rationale: clean process isolation, low coupling to CamillaDSP internals, a simple upgrade path, a smaller piCoreCDSP build, one public API shared by GUI and coordinator, and an easier future rollback of our own workarounds.

---

## 3. Long-Term End Goal

The ideal end state is:

```text
Producer → pcm.camilladsp → snd-aloop → CamillaDSP → DAC
```

Long-term, piCoreCDSP should consist of little more than:

```text
Installer
+ ALSA setup
+ initial configs
+ packaging
+ possibly minimal integration logic
```

The Rust daemon is not an end in itself.

- [ ] Every new upstream capability is evaluated for whether local Rust code can be deleted as a result.
- [ ] No workaround enters the core without a documented deletion path.
- [ ] New upstream capability replaces local code.
- [ ] No permanent duplicate implementations.

---

## 4. Ownership Model

### 4.1 User / CamillaGUI own

Filters, mixer, pipeline, FIR files, playback device, resampler, DSP/output sample rate, `chunksize`, `target_level`, volume, mute, and all other persistent DSP configuration.

### 4.2 ALSA owns

Audio transport, producer active/inactive state, the current nominal source sample rate, the actually negotiated format, and the actually negotiated channel count.

### 4.3 CamillaDSP owns

Capture, playback, DSP processing, buffering, clock drift, rate adjust, config validation, relative paths, `$samplerate$`/token resolution, device restarts, processing state, stop reason, statefile, config file path, and the runtime config lifecycle.

### 4.4 Rust owns only temporarily

Observation of `snd-aloop`, active/inactive detection, detection of the nominal source rate, reconciliation between ALSA and CamillaDSP, temporary source-rate synchronization, bounded retry/backoff, diagnostics, and workarounds for capabilities upstream does not yet provide.

**Central rule:**

> **User config wins on configuration. ALSA wins on source rate. CamillaDSP owns the DSP lifecycle wherever upstream already can.**

---

## 5. Producer-Independent Audio Ingress

piCoreCDSP knows no concrete producers (e.g. Squeezelite, AirPlay/Shairport Sync, other ALSA applications). All producers use the same ingress: `pcm.camilladsp`.

Rust never gets a producer-specific abstraction such as `enum Producer { Squeezelite, AirPlay }`. Instead, exclusively:

```rust
enum SourceState {
    Inactive,
    Active { sample_rate: u32 },
}
```

- [ ] No Squeezelite-specific core logic.
- [ ] No AirPlay-specific core logic.
- [ ] No producer detection.
- [ ] No producer priority logic in Rust.
- [ ] No audio mixing in piCoreCDSP.
- [ ] Exactly one producer owns the ingress at a time.
- [ ] Producer arbitration stays outside piCoreCDSP.
- [ ] A new ALSA producer must never require a Rust code change.

---

## 6. ALSA Ingress Contract

```text
Producer → pcm.camilladsp → ALSA plug → snd-aloop
```

Target configuration:

```text
pcm.camilladsp {
    type plug
    slave {
        pcm "hw:Loopback,1,0"
        format S32_LE
        channels 2
        rate unchanged
    }
}
```

Target invariants: `format = S32_LE`, `channels = 2`, `samplerate = unchanged`.

- [ ] Verify `rate unchanged` on the target ALSA setup.
- [ ] Test S16 producer → S32_LE.
- [ ] Test S24 producer → S32_LE.
- [ ] Test S32 producer → S32_LE.
- [ ] Test stereo → stereo.
- [ ] Deliberately define mono → stereo behavior.
- [ ] Set `route_policy` explicitly if necessary.
- [ ] Test 44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz.
- [ ] Ensure no unintended rate resampling occurs.
- [ ] Test concurrent producer open.
- [ ] Test behavior across producer handover.

---

## 7. CamillaDSP Transport Contract

A piCoreCDSP-compatible config must satisfy, in essence:

```yaml
capture:
  type: Alsa
  device: "hw:Loopback,0,0"
  channels: 2
  format: S32_LE
  stop_on_inactive: true
```

Rust may: read it, validate it, log errors, and suspend managed mode on incompatibility.

Rust may **not**: repair the capture type, overwrite the capture device, automatically change channels or format, automatically set `stop_on_inactive`, or write user YAML back.

On an incompatible config: `Managed mode suspended → clear error → wait for user change`.

---

## 8. Five Separate State Truths

There is no single "config truth" in the system.

1. **Source Transport State** — source: `snd-aloop` HCTL. Provides `Active`, `Rate`, `Format`, `Channels`. ALSA is the truth about the current source.
2. **DSP Process State** — source: CamillaDSP. States: `Offline`, `Starting`, `Running`, `Paused`, `Stalled`, `Inactive`, `Failed`.
3. **Applied Runtime Config** — source: `GetConfig` / `GetPreviousConfig`. Represents the last user decision actually applied at runtime (e.g. GUI `gain = +6 dB`, Apply, no Save → `+6 dB` is authoritative during operation, independent of the on-disk file).
4. **Persistent Config State** — source: `ConfigFilePath` / statefile / config file. This is the bootstrap/reboot truth, not automatically the current runtime truth.
5. **GUI Draft State** — not-yet-applied GUI changes. Rust does not know this state and should not know it.

---

## 9. Hard Config Invariants

- [ ] Rust never writes user YAML.
- [ ] No `runtime.yml`.
- [ ] No shadow config file.
- [ ] No FIR path rewriting.
- [ ] No general config adaptation engine.
- [ ] `Save != Apply`.
- [ ] Save without Apply does not change the running DSP.
- [ ] Apply without Save is a legitimate runtime truth.
- [ ] Normal rate changes must preserve Apply-without-Save.
- [ ] GUI Apply must never be rolled back by Rust.
- [ ] Config switches must never be reverted by Rust.
- [ ] `ConfigFilePath` may diverge from the runtime config; this divergence is not an error.
- [ ] The on-disk file is not the template for normal rate changes.

---

## 10. Reconciliation Instead of a Historical State Machine

```text
Trigger → read current overall state → determine desired state
→ minimal necessary action → settle → re-read current overall state → verify
```

Triggers today: ALSA HCTL event, CamillaDSP polling, retry timer.
Triggers with 4.2/5: ALSA HCTL event, CamillaDSP `SubscribeState`, retry timer, slow safety reconcile.

**Guiding rule:**

> **Events carry no truth. They only cause a fresh snapshot.**

---

## 11. `snd-aloop` Source Observer

Rust reads: PCM slave active, PCM slave rate, PCM slave format, PCM slave channels.

- [ ] Open HCTL non-blocking.
- [ ] Subscribe to events.
- [ ] Debounce briefly after an event.
- [ ] Re-read a full snapshot afterward.
- [ ] Never treat the event payload itself as final truth.
- [ ] Test ~50 ms as the initial debounce.
- [ ] Keep a slow periodic safety snapshot.
- [ ] Treat format/channels as transport invariants.
- [ ] No config mutation based on format/channels.

---

## 12. Normal Stop Lifecycle

```text
Producer stops → PCM slave active = false → CamillaDSP stop_on_inactive → capture is released
```

- [ ] Rust performs no own stop in the normal case.
- [ ] Rust gives CamillaDSP a short grace/settle phase.
- [ ] Rust then checks real state.
- [ ] Safety stop only on unexpected hangs.
- [ ] No config change on a normal stop.

---

## 13. Settled-State Concept

No writes directly on raw state changes.

```text
State event → debounce → fresh read → transition still active? → yes: read again later / no: settled
```

A state is considered settled when: CamillaDSP is no longer Starting/Stopping, the ALSA snapshot is stable, the expected `GetConfig`/`GetPreviousConfig` is available, and no immediately newer event has arrived.

No long fixed sleeps — instead: short debounce, fresh read, optionally a second fresh read.

---

## 14. Runtime Config Priority

- DSP Running/Paused → source: `GetConfig`.
- DSP settled Inactive after normal stop → source: `GetPreviousConfig`.
- Cold boot without runtime history → source: statefile / `ConfigFilePath` / file.

Priority order: Active Runtime Config → Previous Runtime Config → Persistent File.

---

## 15. Source-Rate Policy

Exactly two cases:

- **No resampler:** `devices.samplerate = current_source_rate`.
- **Resampler present:** `devices.capture_samplerate = current_source_rate`.

`devices.samplerate` stays user-owned in the resampler case.

Rust never changes: resampler type, resampler quality, DSP output rate, chunksize, target level.

---

## 16. Current Workaround — Rate Sync While Running

While CamillaDSP upstream offers no persistent source-rate override:

```text
Source active, DSP Running/Paused
  → GetConfig FRESH → check transport contract → determine RateTarget
  → rate correct? yes: do nothing / no: SetConfigValue(rate only)
  → settle → fresh read → verify
```

- [ ] Change exactly one rate field only.
- [ ] Never cache config long-term.
- [ ] Only report local "success" after verification.
- [ ] Re-check source rate after write.
- [ ] Re-check runtime config after write.

---

## 17. Current Workaround — Rate Sync After Inactive

While CamillaDSP offers no source-rate override in the inactive state:

```text
Source active, DSP settled Inactive
  → GetPreviousConfig FRESH → check transport contract
  → change exactly one rate field → SetConfig → settle → verify
```

This is meant to preserve: filter changes, mixer changes, pipeline changes, Apply-without-Save, and the currently applied config selection.

---

## 18. Cliffhanger A — No Native Runtime Source-Rate Override

Today piCoreCDSP must: read the runtime config, determine the rate target, change the rate field, and re-apply `PreviousConfig` when inactive. This code lives exclusively in `rate_sync/`.

```rust
trait SourceRateSynchronizer {
    async fn ensure_source_rate(&self, source_rate: u32, snapshot: &DspSnapshot) -> Result<()>;
}
```

**Removal criterion:** once CamillaDSP upstream provides a source-rate override that can be set in the inactive state, survives reload, survives `SetConfig`, survives GUI Apply, survives config switches, correctly handles the resampler, and honors `$samplerate$` before token resolution → `ConfigPatchRateSynchronizer → DELETE`.

---

## 19. Cliffhanger B — No Native Loopback Rate Following

Today Rust observes `snd-aloop` HCTL for Active/Rate. This code lives exclusively in `source/alsa_loopback.rs`.

```rust
trait SourceObserver {
    async fn snapshot(&self) -> Result<SourceSnapshot>;
    async fn next_trigger(&mut self) -> Result<()>;
}
```

**Removal criterion:** once CamillaDSP upstream reliably detects loopback active, reads the current source rate itself, processes rate changes itself, releases capture on inactive, and starts the new rate itself → `source/alsa_loopback.rs → DELETE`. If CamillaDSP takes over the complete lifecycle → `rate_sync` + large parts of `reconcile → DELETE`, and subsequently the Rust daemon itself may potentially be deleted.

---

## 20. Cliffhanger C — No Config Revision / CAS

GUI Apply B and Rust `SetConfig(A + new_rate)` can occur nearly simultaneously; there is no documented atomic compare-and-swap semantics.

- [ ] Read config fresh immediately before write.
- [ ] Never cache the runtime config across long transitions.
- [ ] Re-check the source immediately before write.
- [ ] Use a runtime fingerprint.
- [ ] Fresh-read after write.
- [ ] Discard local work on mismatch and reconcile from scratch.
- [ ] Never treat `RateLimitExceeded`/disconnect as a partial success.

**Product guarantee:** concurrent GUI edits and source-rate transitions converge to the latest observable state. Exact simultaneous writes are not strictly transactional with the current API.

**Removal criterion:** once upstream provides config generation/revision, compare-and-swap, optimistic concurrency, or an atomic source-rate overlay, race mitigation can be significantly reduced or deleted.

---

## 21. Cliffhanger D — `$samplerate$`-Materialized Resources

`fir_$samplerate$.wav` can be materialized on load into `fir_44100.wav`; subsequent rate patching no longer knows the original token.

v2 policy:

- [ ] Do not fully support `$samplerate$`-dependent resources in native-rate mode initially.
- [ ] Detect known cases.
- [ ] Do not silently continue incorrectly — fail closed with an understandable message.
- [ ] Do not build our own token/template engine.
- [ ] Document separate configs as an alternative.
- [ ] Document a fixed DSP rate + resampler as an alternative.

**Removal criterion:** once CamillaDSP offers a real source-rate override applied before token/path resolution → `token guard → DELETE`.

---

## 22. Cliffhanger E — DSP State Polling

CamillaDSP 4.2/5 offers `SubscribeState`.

```rust
trait DspTriggerSource {
    async fn next_trigger(&mut self) -> Result<()>;
}
```

Development path: 4.1 → polling; 4.2/5 → `SubscribeState` + slow safety reconcile.

**Removal criterion:** once our production baseline reliably supports state push → `fast DSP state poller → DELETE`. The slow safety reconcile stays.

---

## 23. Fixed Camilla Abstraction Boundary

The reconciler knows nothing about the WebSocket wire format. Semantic API:

```rust
trait CamillaControl {
    async fn version(&self) -> Result<Version>;
    async fn state(&self) -> Result<DspState>;
    async fn stop_reason(&self) -> Result<Option<StopReason>>;
    async fn active_config(&self) -> Result<Option<ConfigDocument>>;
    async fn previous_config(&self) -> Result<Option<ConfigDocument>>;
    async fn config_file_path(&self) -> Result<Option<PathBuf>>;
    async fn set_config(&self, config: &ConfigDocument) -> Result<()>;
    async fn set_config_value(&self, path: &str, value: Value) -> Result<()>;
    async fn stop(&self) -> Result<()>;
}

trait CamillaStateEvents {
    async fn subscribe_state(&self) -> Result<StateStream>;
}
```

---

## 24. WebSocket v4/v5 Strategy

CamillaDSP 5 has an incompatible wire format. During development:

```text
camilla/
├── mod.rs
├── protocol_v4.rs
└── protocol_v5.rs
```

- [ ] Both implement the same semantic API.
- [ ] No version checks in the reconciler.
- [ ] No version checks in the ALSA code.
- [ ] No wire-format details in `config_view`.
- [ ] No direct JSON dependency outside the adapter.
- [ ] 4.2 and `next5` are tested in parallel as canaries.

But: **no permanent multi-version production.** Before product release: exactly one production adapter. After the final v5 upgrade: `protocol_v4.rs → DELETE`.

---

## 25. `camillalib` Explicitly Not Used

- [ ] No `camillalib` dependency.
- [ ] CamillaDSP is not embedded into piCoreCDSP.
- [ ] `run_engine()` is never started from piCoreCDSP.
- [ ] No internal engine channels are used.
- [ ] No CamillaDSP shared-state structures are used directly.
- [ ] No coupling to CamillaDSP's internal Rust schema.
- [ ] No shared failure domain.
- [ ] No shared build/feature matrix.

Re-evaluated only if upstream later explicitly guarantees: a stable embedded engine API, stable SemVer rules, a stable lifecycle API, stable state subscriptions, a stable config API, a stable source-rate API, and clearly documented shutdown/thread semantics.

---

## 26. ConfigDocument Kept Deliberately Schema-Light

Rust does not model the complete CamillaDSP config schema. Internal representation: `ConfigDocument` as a generic YAML/JSON tree. Rust only knows the paths it needs:

```text
devices.samplerate
devices.capture_samplerate
devices.resampler
devices.capture.type
devices.capture.device
devices.capture.channels
devices.capture.format
devices.capture.stop_on_inactive
```

Not modeled: filter models, mixer, processors, biquads, FIR schema, complete v4/v5 config structures.

---

## 27. CamillaDSP 5 as the Design Basis

From the start: WebSocket encapsulated, state events preferred, no library coupling, config schema known minimally, build/packaging kept separate, production version pinned exactly, no automatic major upgrades, upgrade to 5 treated as a deliberate product gate, GUI compatibility treated as an equally important release gate.

---

## 28. CamillaGUI Strategy

No piCoreCDSP-specific fork.

- [ ] Use the native CamillaDSP statefile.
- [ ] Remove custom `on_get_active_config`.
- [ ] Remove custom `on_set_active_config`.
- [ ] Remove the shadow `active_config.yml` unless technically mandatory.
- [ ] Rust never observes CamillaGUI directly.
- [ ] All GUI changes are observed exclusively via CamillaDSP.
- [ ] GUI Apply may happen completely independently of the Rust controller.
- [ ] Validate a compatible CamillaGUI/pyCamillaDSP stack before the v5 release.

---

## 29. GUI Operating States

- **Apply during playback:** Rust initially changes nothing, fresh reconciles, only re-aligns source rate if needed, preserves all other GUI changes.
- **Apply without Save:** RuntimeConfig wins; normal rate changes must preserve it; prefer `GetConfig`/`GetPreviousConfig`; do not reload the file.
- **Save without Apply:** the running DSP stays unchanged; a file modification triggers no automatic reload; Rust must never promote a saved draft to the runtime state.
- **Config A → Config B:** the most recently applied user decision wins; Rust must never restore A; the current source rate is reconciled onto B.
- **`ConfigFilePath != RuntimeConfig`:** a legitimate state, no repair, no warning solely because of this divergence.

---

## 30. Normal Rate Change

```text
44.1 kHz → producer stop → CamillaDSP Inactive → PreviousConfig available
→ new producer at 96 kHz → Rust reads PreviousConfig fresh
→ adjust rate only → SetConfig → settle → verify
```

**Mandatory regression test:**

```text
Disk: gain = 0
Playback: 44.1 kHz
GUI: gain = +6, Apply, NO SAVE
Source: 44.1 → 96 → 48

Expected: gain stays +6, source rate follows 44.1 → 96 → 48
```

- [ ] Must pass.

---

## 31. New Source With the Same Rate

Even `48 kHz → inactive → 48 kHz` is a new source lifecycle.

- [ ] Detect the active generation, not just `old_rate != new_rate`.
- [ ] Restart the DSP from `PreviousConfig` if needed.
- [ ] Change rate only if necessary.

---

## 32. Concurrent Apply + Rate Change

The hardest race case.

```text
Trigger → settle → source fresh → config fresh → re-check immediately before write
→ perform minimal rate write → settle → fresh read → verify
```

- [ ] Never blindly resend an old config.
- [ ] Never cache config for seconds.
- [ ] No retry with a stale payload.
- [ ] Reconcile from scratch when in doubt.

---

## 33. Error and Recovery Model

- **WebSocket offline:** bounded backoff, reconnect, then a full fresh snapshot.
- **DAC unavailable:** log it, retry with the current RuntimeConfig, no automatic DAC switch.
- **Invalid config:** do not repair; the old valid RuntimeConfig remains authoritative as long as CamillaDSP keeps it, otherwise `WaitingForUserFix`.
- **Incompatible transport config:** suspend managed mode, clear error message, wait for a new Apply.
- **Stalled:** a short observation phase, no immediate restart loop, re-check the source, check `StopReason`, bounded retry.
- **Rust crash:** CamillaDSP keeps running independently; Rust restarts stateless; source and DSP are read fresh; reconcile.
- **CamillaDSP crash:** Rust stays alive; the CamillaDSP process is restarted (or external service management takes over); unsaved RuntimeConfig may be lost — this is an accepted v2 MVP boundary.
- **CamillaGUI crash:** audio unaffected, Rust unaffected, CamillaDSP unaffected.

---

## 34. Cold Boot

Preferred: `camilladsp -w -s statefile.yml`. `--no_config` is not a fixed architectural requirement.

Test matrix: boot without producer; statefile config present; `stop_on_inactive` leads to clean inactive; `PreviousConfig` available afterward; boot with an already-active producer at the same rate; boot with an already-active producer at a different rate; startup `CaptureError`; startup `PlaybackError`; missing `ConfigFilePath`; invalid persistent config; no statefile.

---

## 35. No Normal Disk Config Watcher

Not to be rebuilt: mtime watcher, inode watcher, file-fingerprint-based auto reload. Reason: `Save != Apply`. A runtime fingerprint is used only for race detection (`hash(GetConfig)`, `hash(GetPreviousConfig)`), never for automatic file synchronization.

---

## 36. Recommended Module Structure

```text
src/
├── main.rs
├── reconcile.rs
│
├── source/
│   ├── mod.rs
│   └── alsa_loopback.rs
│
├── camilla/
│   ├── mod.rs
│   ├── protocol_v4.rs
│   └── protocol_v5.rs
│
├── rate_sync/
│   ├── mod.rs
│   └── config_patch.rs
│
├── config_view.rs
├── retry.rs
├── error.rs
└── logging.rs
```

Expected future rollback: `protocol_v4.rs → DELETE`; `config_patch.rs → DELETE`; `alsa_loopback.rs → possibly DELETE`; `reconcile.rs → possibly much smaller`.

---

## 37. Reconcile Pseudocode

```text
trigger

source = source_observer.snapshot()
dsp    = camilla.snapshot()

if source inactive:
    wait for stop_on_inactive
    if settled and DSP still running:
        safety recovery
    return

if source transport invalid:
    report
    suspend managed mode
    return

if DSP transitioning:
    reconcile later
    return

if DSP running or paused:
    cfg = GetConfig fresh
    validate transport
    rate_sync.ensure_source_rate(source.rate, cfg)
    verify later
    return

if DSP settled inactive:
    cfg = GetPreviousConfig fresh
          or bootstrap source if none
    validate transport
    rate_sync.start_with_source_rate(source.rate, cfg)
    verify later
    return

if DSP failed:
    classify
    bounded retry
    full fresh snapshot on every attempt
```

---

## 38. Upstream Capability Matrix

| Capability | 4.1 | 4.2 | 5 (current) | Our workaround |
|---|---:|---:|---:|---|
| State push events | no | yes | yes | polling fallback |
| `stop_on_inactive` | yes | yes | yes | use it |
| `GetConfig` | yes | yes | yes | use it |
| `GetPreviousConfig` | yes | yes | yes | use it |
| `SetConfigValue` | yes | yes | yes | use it |
| Persistent source-rate override | no | no | currently no | `rate_sync/config_patch` |
| Source-rate override in inactive state | no | no | currently no | PreviousConfig + SetConfig |
| Native aloop rate following | no | no | currently no | `source/alsa_loopback` |
| Config revision/CAS | no | no | currently no | fresh reads + verify |
| Source-rate-aware token re-resolution | no (no runtime API) | no | currently no | feature limited |

This matrix is not a compatibility promise — it exists solely so our own workarounds can be deleted precisely when upstream catches up.

---

## 39. Upstream Removal Matrix

| Upstream capability | Local code removed |
|---|---|
| stable `SubscribeState` | fast DSP state poller |
| persistent runtime source-rate override | config rate patching |
| override in inactive state + survives SetConfig | PreviousConfig rate rebuild |
| token-aware source-rate override | `$samplerate$` guard |
| native aloop rate detection | HCTL rate observer |
| native aloop restart lifecycle | rate reconciler |
| complete source lifecycle | Rust daemon — re-evaluate, possibly delete |
| config revision/CAS | reduce race mitigation |
| stable embedded API | re-evaluate only, not automatic migration |

---

## 40. 4.2 / 5 Development Strategy

During development: CamillaDSP 4.2 as canary, CamillaDSP `next5` as canary, the same reconciler against both, protocol differences confined to the adapter, no user-config migration in Rust, no automatic production switch to development branches.

**Release gate:** Is CamillaDSP 5 official? Is CamillaGUI/pyCamillaDSP compatible? Is ARM/pCP packaging validated? Are our lifecycle tests green?

- If yes: `v2 → CamillaDSP 5`, `protocol_v4.rs → DELETE`.
- If no, but 4.2 is production-ready: `v2 → CamillaDSP 4.2`, `protocol_v5` stays canary. On the later 5 upgrade: test the v5 adapter → switch the production stack → delete the v4 adapter.

---

## 41. CI Strategy

**Release-gate CI** (only the pinned production stack): fmt, clippy, unit tests, integration tests, ARM build, ALSA state tests, Camilla protocol tests, config continuity tests, race tests, failure recovery tests.

**Upstream canary CI** (not release-blocking): `next4.2.0`, `next5`, upcoming GUI branches, WebSocket contract probe, config capability probe, aloop lifecycle probe, state event probe. The canary reports "upstream capability changed" but never auto-updates the product.

---

## 42. Black-Box Capability Probes

Instead of only watching source-code diffs, probe: can CamillaDSP detect loopback rate itself? can source rate be set in the inactive state? does a rate override persist across `SetConfig`? across GUI Apply? across config switches? are `$samplerate$` tokens re-evaluated correctly? does `stop_on_inactive` reliably release loopback? does `GetPreviousConfig` stay correct after a normal stop? does `ConfigFilePath` stay unchanged across `SetConfig`? does config revision/CAS exist? does `SubscribeState` work reliably? does CamillaDSP have a full native aloop rate lifecycle?

When a probe first turns green upstream → check the Removal Matrix → delete the workaround.

---

## 43. Installer v2

Fresh and minimal:

- [ ] Fresh install only.
- [ ] Check `snd-aloop`.
- [ ] Detect the physical playback device once.
- [ ] Install `pcm.camilladsp`.
- [ ] Install the pinned CamillaDSP.
- [ ] Install a compatible, pinned CamillaGUI.
- [ ] Configure a shared native statefile.
- [ ] Generate `Bypass.yml` only if absent.
- [ ] Generate `Null.yml` only if absent.
- [ ] Install Rust v2 as long as it's still needed.
- [ ] Route producers to `pcm.camilladsp`.
- [ ] No Squeezelite parameters as a core prerequisite.
- [ ] No backend menu.
- [ ] No backend switcher.
- [ ] No reinstall migration.
- [ ] Never overwrite an existing user config.
- [ ] Run pCP backup.
- [ ] Reboot if required.

---

## 44. Bypass / Null Configs

- [ ] Generate matching the pinned CamillaDSP version.
- [ ] Correct loopback capture.
- [ ] `S32_LE`, 2 channels, `stop_on_inactive: true`.
- [ ] Detected physical playback device.
- [ ] Sensible rate-adjust defaults.
- [ ] No piCoreCDSP runtime tokens.
- [ ] User-owned after installation, never automatically rewritten.
- [ ] Validate a new v5 schema separately.
- [ ] Never silently migrate existing v4 user configs.

---

## 45. Mandatory Test Matrix — Source

- [ ] Boot without source.
- [ ] Boot with source.
- [ ] Start at 44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz.
- [ ] Stop.
- [ ] New source, same rate.
- [ ] New source, different rate.
- [ ] Rapid flapping.
- [ ] Lost HCTL event.
- [ ] Duplicate HCTL events.

## 46. Mandatory Test Matrix — Producer

- [ ] Squeezelite.
- [ ] AirPlay/Shairport.
- [ ] Squeezelite → AirPlay.
- [ ] AirPlay → Squeezelite.
- [ ] Same rate on switch.
- [ ] Different rate on switch.
- [ ] Parallel open.
- [ ] Producer terminates unexpectedly.
- [ ] Producer reopens immediately.

## 47. Mandatory Test Matrix — GUI

- [ ] Filter Apply.
- [ ] Mixer Apply.
- [ ] Pipeline Apply.
- [ ] Config A → B.
- [ ] Apply without Save.
- [ ] Save without Apply.
- [ ] Apply + Save.
- [ ] Apply while source stops.
- [ ] Apply while source starts.
- [ ] Apply during a rate change.
- [ ] Config switch during a rate change.
- [ ] Enable resampler during playback.
- [ ] Disable resampler during playback.
- [ ] GUI restart during playback.

Most important regression (see §30): gain applied without save must survive rate changes 44.1 → 96 → 48.

- [ ] Must pass.

## 48. Failure Injection

- [ ] WebSocket disconnect.
- [ ] CamillaDSP controlled restart.
- [ ] CamillaDSP crash.
- [ ] Rust restart.
- [ ] Rust crash.
- [ ] GUI restart.
- [ ] DAC disconnect/reconnect.
- [ ] Invalid config.
- [ ] Incompatible transport config.
- [ ] Stalled.
- [ ] `PlaybackError` / `CaptureError` / `CaptureFormatChange`.
- [ ] Missing `ConfigFilePath`.
- [ ] Missing statefile.
- [ ] Config file changed externally.
- [ ] `snd-aloop` missing.
- [ ] Loopback handle hangs.
- [ ] WebSocket `RateLimitExceeded`.
- [ ] CamillaDSP still starting while a new event arrives.

---

## 49. Hardware Gate

Before v1→v2 promotion, validate on: real pCP target version, real Raspberry Pi target hardware, real USB DACs, I2S DAC if relevant, multiple producers, both rate families, hundreds of rate changes, hundreds of producer handovers, long-run playback, intensive GUI use, failure injection — with the outcome that no user file was modified, no shadow config was created, no loopback handles hang, no stale runtime rate remains, no applied GUI changes are lost on normal rate changes, and no unintended CamillaDSP process restarts happen on normal filter changes.

---

## 50. Hard v1 → v2 Cleanup

After passing hardware validation:

- [ ] Tag `v1-final`.
- [ ] Optional archive branch.
- [ ] Delete ioplug, C code, IPC, stdin capture, ring buffer, audio worker threads, backend abstraction, backend switcher, `adaptation.rs`, RuntimeConfig, runtime YAML, reinstall logic, ioplug benchmarks, ioplug CI, obsolete upstream monitoring, obsolete docs.
- [ ] README targets piCoreCDSP v2 exclusively.

No `legacy/`, `deprecated/`, `old_controller/`, or `experimental_ioplug/` directories — **git is the archive**.

---

## 51. Definition of Done

piCoreCDSP v2 is done when: the architecture is aligned with CamillaDSP 5; the production stack is exactly pinned; CamillaDSP remains a separate process; `camillalib` is not embedded; exactly one production Camilla WebSocket adapter exists; `pcm.camilladsp` is producer-agnostic; only `snd-aloop` is used; no ioplug exists; no custom audio data path exists; Rust processes no samples; user YAML stays untouched; no runtime YAML exists; `GetConfig`/`GetPreviousConfig` model runtime continuity; Apply-without-Save survives normal rate changes; Save-without-Apply is never auto-applied; config switches work during playback; source rate follows ALSA; native mode works; resampler mode works; GUI/rate races reliably converge; errors are not masked by config repair; every workaround has a removal criterion; upstream canaries detect future simplifications; and v1/ioplug is completely removed from `main`.

---

## 52. Long-Term Deletion Stages

1. Stable state events → delete fast DSP polling.
2. Runtime source-rate override → delete manual config rate patch.
3. Override also while inactive → delete PreviousConfig rate rebuild.
4. Native loopback rate detection → delete the Rust HCTL rate observer.
5. Native loopback lifecycle → delete `rate_sync` + large parts of `reconcile`.
6. Complete upstream solution → delete the Rust daemon.

End state: `Producer → pcm.camilladsp → snd-aloop → CamillaDSP → DAC`. piCoreCDSP then remains essentially: installer + ALSA setup + initial configs + packaging.

---

## 53. Core Philosophy

> **piCoreCDSP v2 is not a new permanent controller platform. It is a small, replaceable compatibility bridge between ALSA source state and the CamillaDSP capabilities available today.**

> **CamillaDSP remains an independent process. piCoreCDSP integrates through public ALSA and WebSocket boundaries, never through unstable internal engine APIs.**

> **Every workaround must have a deletion path. Upstream capabilities replace local code; they do not get added beside it forever.**

These three rules govern the entire new development effort.

---

## 54. Automatic Upstream Monitoring

Upstream monitoring is part of the architecture, not merely a maintenance tool. The goal is not just "upstream repository changed" but **"a change affects a capability piCoreCDSP implements today or could hand off to upstream in the future."** The monitoring is therefore built **capability-aware**:

```text
Upstream source change → mirror relevant files → determine affected capability
→ static contract checks → black-box capability probes → status/report/PR
→ check Removal Matrix
```

---

## 55. Upstream Sources — Priority A: Directly Production-Critical

### 55.1 CamillaDSP Engine (`HEnquist/camilladsp`)

Watch: `master`, `next4.2.0`, `next5`, releases, open PRs against `master`.

Relevant paths: `src/alsa_backend/`, `src/config/`, `src/engine.rs`, `src/websocket_server/`, `src/statefile.rs`, `src/bin.rs`, `backend_alsa.md`, `websocket.md`, `CHANGELOG.md`, `Cargo.toml`, `README.crates.md`.

Capabilities: `camilla.websocket.protocol`, `camilla.state.events`, `camilla.runtime.active_config`, `camilla.runtime.previous_config`, `camilla.runtime.config_path`, `camilla.config.set`, `camilla.config.set_value`, `camilla.source_rate.override`, `camilla.alsa.loopback.active`, `camilla.alsa.loopback.rate`, `camilla.alsa.loopback.lifecycle`, `camilla.stop_on_inactive`, `camilla.config.revision`, `camilla.token.samplerate`, `camilla.statefile`, `camilla.process.lifecycle`.

Currently directly used: WebSocket control API, processing state, `StopReason`, `GetConfig`, `GetPreviousConfig`, `GetConfigFilePath`, `SetConfig`, `SetConfigValue`, `stop_on_inactive`, statefile.

Future-relevant: runtime source-rate override, source-rate override in inactive state, native `snd-aloop` rate detection, native loopback restart lifecycle, config revision/CAS, token-aware runtime overrides, further state push events.

### 55.2 CamillaGUI Backend (`HEnquist/camillagui-backend`)

Watch: `master`, `next4.2.0`, releases, future `next5`/v5 branches.

Relevant paths: `backend/`, `config/`, `release_automation/`, `main.py`, `README.md`, especially `backend/filemanagement.py`, `backend/settings.py`, `backend/settings_schemas.py`, `release_automation/versions.yml`.

Capabilities: `gui.active_config.path`, `gui.statefile.integration`, `gui.apply`, `gui.save`, `gui.eventstream`, `gui.camilla.version_compat`, `gui.config.path_resolution`, `gui.runtime_vs_saved_config`.

Directly relevant: native statefile integration, active-config behavior, Apply/Save semantics, `ConfigFilePath` behavior, CamillaDSP/pyCamillaDSP compatibility versions. Future-relevant: runtime-vs-persistent config display, runtime override awareness, native state event integration, CamillaDSP 5 compatibility.

### 55.3 CamillaGUI Frontend (`HEnquist/camillagui`)

Watch: `master`, `next4.2.0`, releases, future `next5`/v5 branches. Lower priority than the backend, but important for: Apply workflow, config-switch workflow, runtime-state display, new UI semantics for runtime overrides, breaking UI/API assumptions between GUI and backend. Do not fully mirror — only metadata, release info, and specifically relevant UI/API files.

---

## 56. Upstream Sources — Priority B: Reference and Future Removal Signals

### 56.1 Official CamillaDSP Controller (`HEnquist/camilladsp-controller`)

Watch: `main`, releases/tags if present. Relevant files: `alsa_listener.py`, `controller.py`, `config_provider.py`. Not a production dependency — serves as an **upstream reference implementation** for ALSA HCTL monitoring, debounce behavior, `PCM Slave Active`, `PCM Slave Rate`, format/channels snapshot, source-transition semantics, CamillaDSP runtime coordination.

Capabilities: `reference.alsa_listener`, `reference.aloop.snapshot`, `reference.debounce`, `reference.rate_switch`, `reference.config_provider`.

If upstream introduces new robust loopback strategies here → review required, compare against `source/alsa_loopback.rs`. Never automatically adopt code.

### 56.2 ALSA Userspace (`alsa-project/alsa-lib`)

Branch: `master`. Relevant paths: `src/pcm/pcm_plug.c`, `src/pcm/`, `src/control/`, `include/`, `doc/asoundrc.txt`.

Capabilities: `alsa.plug.format`, `alsa.plug.channels`, `alsa.plug.rate_unchanged`, `alsa.plug.route_policy`, `alsa.hctl`, `alsa.control.events`.

Directly used: `type plug`, format normalization, channel normalization, `rate unchanged`, HCTL/control API. Monitor: `plug` negotiation changes, `rate unchanged` changes, channel routing changes, HCTL/event API changes, relevant ABI/API changes.

### 56.3 Linux `snd-aloop` — Canonical Upstream (`torvalds/linux`)

Relevant file: `sound/drivers/aloop.c`. Optional: `Documentation/sound/`.

Capabilities: `kernel.aloop.active`, `kernel.aloop.rate`, `kernel.aloop.format`, `kernel.aloop.channels`, `kernel.aloop.pcm_notify`, `kernel.aloop.release_semantics`.

Watch: `PCM Slave Active`, `PCM Slave Rate`, `PCM Slave Format`, `PCM Slave Channels`, `pcm_notify`, `snd_ctl_notify`, close/open and format-change semantics. This source tells us what Linux upstream can do in general — not automatically what's already available on piCorePlayer.

---

## 57. Upstream Sources — Priority A for the Real Target Platform

### 57.1 piCorePlayer Linux Kernel (`piCorePlayer/linux`)

Important because this is more relevant for production decisions than `torvalds/linux` alone — it reflects the actual kernel state pCP ships/patches. Relevant file: `sound/drivers/aloop.c`.

Capabilities: `pcp.kernel.aloop.active`, `pcp.kernel.aloop.rate`, `pcp.kernel.aloop.pcm_notify`, `pcp.kernel.version`.

Check: does `snd-aloop` match canonical upstream? are relevant new aloop patches missing? are there pCP-specific changes? what kernel version is the current production target?

### 57.2 piCorePlayer Kernel Config / Symbols (`piCorePlayer/pCP-Kernels`)

Relevant for: `CONFIG_SND_ALOOP`, module availability, armv7/aarch64 architecture, kernel ABI, kernel module packaging.

Capability: `pcp.snd_aloop.available`. Installer gate: `snd-aloop` unavailable → abort installation.

### 57.3 piCorePlayer Releases (`piCorePlayer/pCP-Releases`)

Relevant for: new pCP versions, image/architecture changes, kernel changes, possible changes to supported platforms. Official pCP documentation can additionally be observed as an external release source, but the GitHub workflow should primarily rely on GitHub sources.

---

## 58. Sources We Should Not Fully Mirror

Not needed as a full mirror: `torvalds/linux` in full, `piCorePlayer/linux` in full, the complete `camillagui` frontend, `pycamilladsp-plot`, CamillaDSP benchmarks unrelated to our capabilities, generic DSP filter implementations unrelated to our contract.

Instead: **sparse upstream snapshot + immutable source metadata.** Every snapshot contains: repository, ref, commit SHA, fetched-at timestamp, relevant paths, release/tag metadata.

---

## 59. Proposed Repository Structure for the Upstream Mirror

```text
upstream/
├── manifest.yml
├── status.json
├── capabilities.yml
│
├── camilladsp/
│   ├── master/
│   ├── next4.2.0/
│   └── next5/
│
├── camillagui-backend/
│   ├── master/
│   └── next4.2.0/
│
├── camillagui/
│   ├── master/
│   └── next4.2.0/
│
├── camilladsp-controller/
│   └── main/
│
├── alsa-lib/
│   └── master/
│
├── linux-aloop/
│   └── master/
│
├── pcp-linux-aloop/
│   └── current/
│
├── pcp-kernels/
│   └── current/
│
└── pcp-releases/
    └── current/
```

Only relevant files are stored.

---

## 60. `upstream/manifest.yml`

The mirror is controlled declaratively. Example:

```yaml
version: 1

sources:
  - id: camilladsp-next5
    repo: HEnquist/camilladsp
    ref: next5
    priority: critical
    paths:
      - src/alsa_backend/**
      - src/config/**
      - src/engine.rs
      - src/websocket_server/**
      - src/statefile.rs
      - src/bin.rs
      - backend_alsa.md
      - websocket.md
      - CHANGELOG.md
      - Cargo.toml
    capabilities:
      - camilla.websocket.protocol
      - camilla.state.events
      - camilla.source_rate.override
      - camilla.alsa.loopback.rate
      - camilla.alsa.loopback.lifecycle
      - camilla.config.revision
      - camilla.token.samplerate

  - id: alsa-lib-master
    repo: alsa-project/alsa-lib
    ref: master
    priority: high
    paths:
      - src/pcm/pcm_plug.c
      - src/control/**
      - doc/asoundrc.txt
    capabilities:
      - alsa.plug.rate_unchanged
      - alsa.plug.channels
      - alsa.plug.route_policy
      - alsa.hctl

  - id: linux-aloop
    repo: torvalds/linux
    ref: master
    priority: high
    paths:
      - sound/drivers/aloop.c
    capabilities:
      - kernel.aloop.active
      - kernel.aloop.rate
      - kernel.aloop.pcm_notify

  - id: pcp-linux-aloop
    repo: piCorePlayer/linux
    ref: auto
    priority: critical
    paths:
      - sound/drivers/aloop.c
    capabilities:
      - pcp.kernel.aloop.rate
      - pcp.kernel.aloop.pcm_notify
```

`ref: auto` means: resolve the repo's current default/production branch and store the SHA explicitly in the status file.

---

## 61. `upstream/capabilities.yml`

Every capability is linked to our code. Example:

```yaml
capabilities:

  camilla.state.events:
    used_now: true
    local_code:
      - src/camilla/
      - src/reconcile.rs
    current_fallback:
      - dsp_state_polling
    removal_when:
      - stable_subscribe_state

  camilla.source_rate.override:
    used_now: false
    wanted_future: true
    local_code:
      - src/rate_sync/config_patch.rs
    current_fallback:
      - set_config_value
      - previous_config_rebuild
    removal_when:
      - runtime_override_works_while_inactive
      - override_survives_set_config
      - override_survives_gui_apply
      - override_is_token_aware

  camilla.alsa.loopback.lifecycle:
    used_now: false
    wanted_future: true
    local_code:
      - src/source/alsa_loopback.rs
      - src/rate_sync/
      - src/reconcile.rs
    removal_when:
      - camilla_detects_loopback_rate
      - camilla_restarts_on_new_rate
      - camilla_releases_on_inactive

  alsa.plug.rate_unchanged:
    used_now: true
    local_code:
      - installer/
      - assets/asound.conf
    required_contract:
      - source_rate_is_not_resampled

  kernel.aloop.pcm_notify:
    used_now: false
    wanted_future: true
    local_code:
      - src/source/alsa_loopback.rs
    note:
      - architecture_must_not_depend_on_this_until_hardware_validated
```

This allows a workflow to automatically report, e.g., for a changed upstream file `src/alsa_backend/utils.rs`: affected capabilities `camilla.alsa.loopback.rate`, `camilla.alsa.loopback.lifecycle`; potential local code `src/source/alsa_loopback.rs`, `src/rate_sync/`, `src/reconcile.rs`; removal candidate: yes/no.

---

## 62. GitHub Workflow 1 — `upstream-sync.yml`

Schedule: daily + `workflow_dispatch`.

- [ ] Read `upstream/manifest.yml`.
- [ ] Resolve the current SHA of every source.
- [ ] Fetch only relevant paths.
- [ ] Update the snapshot.
- [ ] Capture release/branch metadata.
- [ ] Update `upstream/status.json`.
- [ ] Classify the diff by capability.
- [ ] No direct pushes to `main`.
- [ ] Create an automatic branch.
- [ ] Open an automatic PR (e.g. `chore(upstream): sync CamillaDSP next5 @ <sha>`), with a body listing changed sources, touched capabilities, potentially affected local areas, capability probe results, and removal candidates.

---

## 63. GitHub Workflow 2 — `upstream-capability-canary.yml`

Trigger: after an upstream-sync PR + nightly + `workflow_dispatch`. Runs black-box probes, not just source analysis.

**CamillaDSP probes:** WebSocket protocol contract, `GetConfig`, `GetPreviousConfig`, `ConfigFilePath`, `SetConfig`, `SetConfigValue`, state events, `stop_on_inactive`, source-rate override present?, override while inactive?, override survives `SetConfig`?, override survives GUI Apply?, config revision/CAS?, `$samplerate$` token-aware override?

**ALSA/kernel probes** (where the CI environment supports it): load `snd-aloop`, HCTL controls present, active transition, rate/format/channel snapshot, close/open rate change, `pcm_notify` capability.

Hardware-dependent probes are additionally run on a real pCP test device and are not simulated by GitHub-hosted CI.

---

## 64. GitHub Workflow 3 — `upstream-release-watch.yml`

Schedule: daily. Watches releases of CamillaDSP, CamillaGUI backend, CamillaGUI, pyCamillaDSP, piCorePlayer. Reports "new release available" with currently pinned version, new version, breaking/not breaking, canary status, production eligibility. Automatic upgrade: no. Instead: release discovered → canary → hardware validation → deliberate product decision.

---

## 65. GitHub Workflow 4 — `upstream-branch-watch.yml`

Especially important during the 4.2/5 transition. Watches newly appearing branches (`next*`, `v5*`, `5.*`), especially on `camillagui-backend`, `camillagui`. Example: `camillagui-backend` gets `next5` → automatic issue/report: "CamillaDSP 5 ecosystem signal detected". This is a strong release-readiness signal.

---

## 66. GitHub Workflow 5 — `upstream-removal-check.yml`

Trigger: when a capability probe flips FAIL → PASS (e.g. `camilla.source_rate.override`). The workflow does not auto-delete code — instead it opens an issue: "Removal candidate: config_patch rate synchronizer", listing the upstream capability, previous/new probe result, potentially removable local code, and required validation before removal (inactive state, GUI Apply, config switch, `$samplerate$`, resampler mode, hardware 44.1/48/96/192). After successful hardware validation → remove the workaround.

---

## 67. Monitoring Levels

- **Critical** (immediate canary + review): CamillaDSP WebSocket, CamillaDSP config lifecycle, CamillaDSP ALSA loopback, `snd-aloop`, pCP kernel/`snd-aloop`, statefile, `GetConfig`/`GetPreviousConfig`, `SetConfig`/`SetConfigValue`.
- **High** (automatic PR + tests): CamillaGUI backend, pyCamillaDSP, alsa-lib plug/HCTL, CamillaDSP controller ALSA listener, pCP kernel config.
- **Medium** (report only): CamillaGUI frontend, docs, packaging-related upstream changes.
- **Ignore unless a dependency changes:** general CamillaDSP filters, unrelated backends, benchmark-only changes, Windows/macOS-only code, unrelated GUI widgets.

---

## 68. Upstream Status Dashboard

Automatically generated file: `upstream/status.md`, summarizing at a glance: what do we use? what can upstream do? what is still missing? which local code does that require? (pinned production version, `next4.2`/`next5` SHAs, capability probe pass/fail per component).

---

## 69. No Automatic Upstream Code Adoption

Mirror explicitly does **not** mean `git subtree merge` of upstream code into production.

- [ ] Upstream snapshots are read-only reference.
- [ ] No automatic adoption of source patches.
- [ ] No automatic config migration.
- [ ] No automatic version upgrade.
- [ ] No automatic deletion of local workarounds.
- [ ] All production changes go through normal review/hardware gates.

The mirror serves detection, analysis, capability verification, and removal planning — not automatic integration.

---

## 70. Retention Policy for the Mirror

Keep in the git repository: the current snapshot, the previous snapshot for diffing, SHA/release history in `status.json`, important capability transitions. Do not keep: full Linux history, full copies of external repositories, binary artifacts, large build outputs. GitHub Actions artifacts may hold more extensive temporary test data.

---

## 71. Automatic Issue Labeling

Recommended labels: `upstream`, `upstream:camilladsp`, `upstream:camillagui`, `upstream:alsa`, `upstream:kernel`, `upstream:pcp`, `capability`, `removal-candidate`, `breaking-change`, `canary-failure`, `release-candidate`.

---

## 72. Upstream Monitoring Definition of Done

- [ ] `upstream/manifest.yml` exists.
- [ ] All sources are assigned a priority.
- [ ] All relevant source paths are defined.
- [ ] Every source maps to at least one capability.
- [ ] Every own compatibility bridge references a capability.
- [ ] Every capability knows the local code it may eventually replace.
- [ ] Daily upstream sync runs.
- [ ] Sync produces a PR instead of a direct main push.
- [ ] Capability canary runs automatically.
- [ ] Release watch runs automatically.
- [ ] Branch watch detects new 4.2/5 ecosystem branches.
- [ ] FAIL→PASS produces a removal-candidate issue.
- [ ] Production upgrade stays manual.
- [ ] Hardware-dependent capabilities have a separate pCP hardware gate.
- [ ] The upstream status dashboard is generated automatically.

---

## 73. Extended Core Philosophy

> **piCoreCDSP monitors upstream not only for new versions, but for new capabilities.**

> **Every local capability today is linked to the upstream capability that should one day replace it.**

> **An upstream update only becomes interesting for piCoreCDSP once a capability probe shows that our concrete system contract has actually changed.**

> **The best upstream outcome is not new code in piCoreCDSP — it is code we can delete from piCoreCDSP.**
