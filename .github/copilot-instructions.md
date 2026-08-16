# GitHub Copilot Instructions

## Roadmap Files Are the Source of Truth

The following files are the authoritative reference for all **new** work in this repository:

- [`piCoreCDSP_v2_Roadmap.md`](../piCoreCDSP_v2_Roadmap.md) — the v2 roadmap: goals, fixed architecture decisions,
  ownership model, state model, reconciliation design, protocol strategy, test matrices, cleanup plan, and upstream
  monitoring strategy.
- [`ROADMAP_CHECKLIST_v2.md`](../ROADMAP_CHECKLIST_v2.md) — the gated checklist tracking progress on each v2
  roadmap item.
- [`docs/new plan/piCoreCDSP_v2_complete_roadmap(1).md`](../docs/new%20plan/piCoreCDSP_v2_complete_roadmap%281%29.md)
  — the original uploaded plan the roadmap above formalizes; consult it if a roadmap section needs more detail
  than the condensed version provides.

`piCoreDSP_Dual_Backend_Roadmap.md` and `ROADMAP_CHECKLIST.md` describe the **superseded v1** dual-backend
(`snd-aloop` + custom `ioplug`) architecture. They are frozen/archival. **Never use them to plan or justify new
work.** Do not resurrect v1 concepts (backend switching, custom ioplug, `RuntimeConfig`/runtime YAML, dual-backend
abstractions) inside v2 code.

**Before writing any code, always:**

1. Read `piCoreCDSP_v2_Roadmap.md` and `ROADMAP_CHECKLIST_v2.md` in full to understand the current state and goals.
2. Identify which roadmap section and checklist gate the work relates to.
3. Confirm the change does not violate a fixed architecture decision (see "Non-Negotiable Architecture Rules" below)
   before implementing it — if it seems to require violating one, stop and raise it instead of working around it
   silently.
4. Ensure your implementation aligns with the roadmap intent, including its ownership model (§4) and the five
   separate state truths (§8).

## Non-Negotiable Architecture Rules

These rules come directly from the roadmap and must hold for every change, with no exceptions unless the roadmap
itself is formally revised:

- CamillaDSP is always a separate OS process. **Never** add a `camillalib` dependency, embed the CamillaDSP engine,
  or use its internal `SharedConfigs`/`ControllerMessage`/`StatusStructs`/engine channels (roadmap §2, §25).
- piCoreCDSP integrates **only** through the public ALSA boundary (`snd-aloop`) and the public CamillaDSP WebSocket
  control API — never through unstable internal engine APIs.
- piCoreCDSP is producer-agnostic. **Never** add Squeezelite-, AirPlay-, or other producer-specific types, detection,
  or priority logic. The only source abstraction is `SourceState { Inactive, Active { sample_rate } }` (roadmap §5).
- Rust **never** writes user YAML, never creates a `runtime.yml` or shadow config file, and never reverts a GUI Apply
  or config switch. `Save != Apply` must be preserved everywhere (roadmap §9).
- The reconciler is a **reconcile loop**, not a historical state machine: read → determine desired state → minimal
  action → settle → re-read → verify. Events only trigger a fresh snapshot; they are never treated as truth
  themselves (roadmap §10, §37).
- `ConfigDocument` stays schema-light (roadmap §26): only the documented paths are modeled. Do not build a full
  CamillaDSP config schema, filter model, or mixer model in Rust.
- Every WebSocket/protocol-version detail is confined to `camilla/protocol_v4.rs` / `camilla/protocol_v5.rs` behind
  the `CamillaControl` trait (roadmap §23–§24). No version checks may leak into the reconciler, source observer, or
  `config_view`.
- **Every workaround/compatibility bridge must carry an explicit, code-linked removal criterion** registered in
  `upstream/capabilities.yml` (once that infrastructure exists) — see roadmap §16–§22, §61. Do not add a workaround
  "temporarily" without writing down what upstream capability would let it be deleted.
- No permanent `legacy/`, `deprecated/`, `old_controller/`, or `experimental_ioplug/` directories. Git tags/branches
  are the archive (roadmap §50).

## Research Before Implementation

Before implementing any feature, workaround, or fix, you **must** research the relevant upstream project(s) rather
than guessing at behavior. This is required, not optional, because the entire v2 architecture is built around
precisely tracking what upstream can and cannot yet do (roadmap §38, §42, §54–§73):

1. **Read the roadmap sections relevant to the change** in `piCoreCDSP_v2_Roadmap.md`, especially the Upstream
   Capability Matrix (§38), Upstream Removal Matrix (§39), and the specific cliffhanger section (§18–§22) if the
   change touches a known workaround area.
2. **Check the current Upstream Capability Matrix / probes** before assuming a capability is missing — upstream may
   have already added it. If your research reveals the matrix is stale, update it as part of your change.
3. **Research the official upstream sources directly**, matching roadmap §55–§58's priority list:
   - [CamillaDSP](https://github.com/HEnquist/camilladsp) engine (WebSocket protocol, `GetConfig`/`GetPreviousConfig`,
     `stop_on_inactive`, statefile, ALSA backend, source-rate handling).
   - [CamillaGUI backend](https://github.com/HEnquist/camillagui-backend) and
     [CamillaGUI frontend](https://github.com/HEnquist/camillagui) (Apply/Save semantics, active-config handling).
   - [pycamilladsp](https://github.com/HEnquist/pycamilladsp) (the Python client CamillaGUI depends on; new
     WebSocket features often surface here first).
   - [camilladsp-controller](https://github.com/HEnquist/camilladsp-controller) as a **reference-only** upstream
     implementation of ALSA HCTL monitoring/debounce — never copy its code wholesale, only compare approaches.
   - [ALSA (alsa-lib)](https://github.com/alsa-project/alsa-lib) for `pcm_plug`, HCTL, and `rate unchanged` semantics.
   - `snd-aloop`: canonical [`torvalds/linux`](https://github.com/torvalds/linux) `sound/drivers/aloop.c`, and —
     more importantly for production decisions — the actual
     [piCorePlayer kernel](https://github.com/piCorePlayer/linux)/[piCorePlayer/pCP-Kernels](https://github.com/piCorePlayer/pCP-Kernels)
     fork actually shipped on the target platform.
   - [piCorePlayer](https://www.picoreplayer.org/) documentation and
     [piCorePlayer/pCP-Releases](https://github.com/piCorePlayer/pCP-Releases) for platform/packaging constraints.
   - Any other library or API being used.
4. **Verify capability claims with a black-box probe where feasible** (roadmap §42) rather than relying solely on
   source-reading — e.g. actually call the WebSocket API against a real or containerized CamillaDSP build instead of
   assuming behavior from documentation alone.
5. **Search for existing solutions in this codebase** before writing new code, and confirm your approach is
   consistent with patterns already established for the v2 architecture.
6. If research shows an upstream capability now covers a documented workaround, treat this as a **removal
   candidate**: check the Upstream Removal Matrix (roadmap §39) and prefer deleting the local workaround over adding
   new code beside it.

## Modularity & Design Principles

- Keep the module boundaries from roadmap §36 intact: `source/` (ALSA-only), `camilla/` (protocol adapters only),
  `rate_sync/` (rate workarounds only), `reconcile.rs` (orchestration only), `config_view.rs`, `retry.rs`,
  `error.rs`, `logging.rs`. A module boundary exists so that its contents can later be deleted independently — do
  not blur these boundaries for convenience.
- Prefer trait-based seams (`CamillaControl`, `CamillaStateEvents`, `SourceObserver`, `SourceRateSynchronizer`,
  `DspTriggerSource`) over concrete coupling, matching the interfaces defined in the roadmap, so protocol/version
  swaps and eventual deletions stay localized.
- Never mix a workaround's implementation with its "permanent" logic — workaround code must be isolated enough to
  delete in one step once its removal criterion is met.
- Keep `ConfigDocument` and all config handling schema-light; do not grow it into a general-purpose CamillaDSP
  config modeling library.

## Testing Requirements

When implementing any new feature or fixing a bug, you **must** write detailed tests, and they must cover the
relevant scenarios from the roadmap's mandatory test matrices (§45–§48), not just an arbitrary happy path:

1. **Cover the happy path** — verify the feature works correctly under normal conditions.
2. **Cover edge cases** — boundary conditions, empty inputs, maximum values, unexpected states, and specifically the
   race/concurrency cases described in roadmap §20 and §32 (concurrent Apply + rate change, stale config writes).
3. **Cover error paths** — verify failures are handled per the Error & Recovery Model (roadmap §33): WebSocket
   offline, DAC unavailable, invalid config, incompatible transport config, stalled DSP, Rust crash, CamillaDSP
   crash, CamillaGUI crash.
4. **Cover the mandatory regression test** wherever a change touches config/rate handling: gain applied without
   save must survive a source rate change of 44.1 → 96 → 48 kHz (roadmap §30, §47).
5. **Use descriptive test names** — each test name should clearly describe what is being tested and the expected
   outcome.
6. **Keep tests isolated** — tests must not depend on each other or on external state; mock or stub external
   dependencies (ALSA/`snd-aloop`, CamillaDSP WebSocket) as needed. Use `#[ignore]`-gated tests opt-in via an
   environment variable for anything that needs a real CamillaDSP binary or real hardware, consistent with the
   hardware-gate separation in roadmap §49.
7. **Run the full test suite** after adding new tests to confirm nothing is broken.

Tests are a required deliverable for every implementation task — a feature is not complete without them.

## Rust Formatting, Clippy, and Test Failures

After implementing new code, always:

1. Run Rust formatting checks and apply formatting fixes (`cargo fmt`).
2. Run Clippy checks and auto-correct issues when possible (`cargo clippy --fix`), then resolve any remaining
   warnings/errors.
3. If any tests fail, fix the issues and re-run formatting, Clippy, and tests until all checks pass.

## Verification Before Marking Done

A checklist item in `ROADMAP_CHECKLIST_v2.md` must **not** be marked as done until:

1. The code implementing the feature is written and reviewed.
2. The implementation is verified to work correctly — test it or trace through the logic carefully.
3. Edge cases and error paths are handled, including the relevant test-matrix scenarios from roadmap §45–§48.
4. The change does not break existing functionality or violate any rule in "Non-Negotiable Architecture Rules" above.
5. Any workaround introduced has its removal criterion documented (roadmap §16–§22, §61).

Only after this verification should you update the checklist item to `[x]`.
