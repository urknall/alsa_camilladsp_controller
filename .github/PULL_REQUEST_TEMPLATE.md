<!--
  piCoreCDSP is mid-transition to the v2 architecture. Before filling this in, read
  piCoreCDSP_v2_Roadmap.md and ROADMAP_CHECKLIST_v2.md — they are the source of truth for new work.
  piCoreDSP_Dual_Backend_Roadmap.md and ROADMAP_CHECKLIST.md are frozen v1 history; do not use them
  to justify new work.
-->

## Summary

<!-- What does this PR change, and why? Which roadmap section(s) / checklist gate(s) does it relate to? -->

## Workaround / compatibility-bridge disclosure

Does this PR add, extend, or modify a **temporary workaround or compatibility bridge** for a CamillaDSP/ALSA/
upstream limitation (roadmap §16–§22, §61)?

- [ ] No — this PR does not introduce or touch a workaround. (Skip the rest of this section.)
- [ ] Yes — this PR introduces or touches a workaround. Complete the following:

  - **Local code:** <!-- module/file(s) implementing the workaround -->
  - **Removal criterion:** <!-- the exact upstream capability that, once available, lets this code be deleted -->
  - **`upstream/capabilities.yml` entry:** <!-- link the `local_code` / `removal_when` entry once that
    infrastructure exists per Gate 14; until then, state the criterion here explicitly -->

  Per the Non-Negotiable Architecture Rules, no workaround may be merged without an explicit, code-linked
  removal criterion. Do not add a workaround "temporarily" without writing down what upstream capability would
  let it be deleted.

## Architecture guard rails checklist

<!-- Confirm none of these are violated by this PR (see ROADMAP_CHECKLIST_v2.md's "Architectural Guard Rails"). -->

- [ ] No `camillalib` dependency added; CamillaDSP remains a separate process.
- [ ] No producer-specific (Squeezelite/AirPlay/etc.) logic added to `reconcile.rs` or `source/`.
- [ ] No protocol-version checks leak outside `camilla/`.
- [ ] No user YAML is written by Rust; no shadow config / `runtime.yml` introduced.
- [ ] No permanent `legacy/`/`deprecated/` directory introduced (git tags/branches are the archive).
- [ ] No fixed long sleeps used for state settling (debounce + fresh read only).

## Testing

<!-- What tests were added/updated? Which roadmap test-matrix scenarios (§45-§48) does this cover? -->
