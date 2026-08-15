# alsa-lib Upstream Tracking

## Purpose

BlueALSA is a reference, not the real API provider — the dependency that
actually matters is **alsa-lib** itself, because both BlueALSA and
piCoreDSP's ioplug plugin (`picoredsp-ioplug`) are built against its public
`pcm_ioplug.h` interface. An alsa-lib release can change ioplug semantics,
`SND_PCM_IOPLUG_FLAG_*` availability, or `hw_params`/`sw_params` behaviour
that affects piCoreDSP even if BlueALSA itself hasn't changed.

## Maintenance priorities

```text
HIGH
    alsa-lib
    CamillaDSP
    piCorePlayer

MEDIUM
    Linux ALSA (kernel-side, e.g. snd-aloop)
    BlueALSA reference changes

LOW
    unrelated BlueALSA Bluetooth functionality
```

## Repository Details

| Field                | Value                                                      |
|----------------------|---------------------------------------------------------------|
| Repository           | <https://github.com/alsa-project/alsa-lib>                    |
| Tracked signal       | Release tags (alsa-lib does not have "one file" to diff; the ioplug ABI surface is `include/pcm_ioplug.h`, `include/pcm_external.h`, plus the `SND_LIB_VERSION` compatibility macros already handled in `picoredsp-ioplug/src/pcm.c`) |
| Last reviewed tag    | `v1.2.16.1`                                                     |
| Last reviewed commit | [`a7babcb`](https://github.com/alsa-project/alsa-lib/commit/a7babcb8e6361719bf18fa96f11354d125447500) |
| Last review date     | 2026-08-15                                                     |

## Automation

A new alsa-lib release triggers the plugin test suite automatically, even
if no source change is required in this repository, so that a real
regression (not just a stale review-log entry) is caught. This is
implemented in `scripts/check_upstream_tracking.py` /
`.github/workflows/upstream-tracking.yml`:

1. `check_upstream_tracking.py` compares `last_reviewed_tag` in
   `docs/upstream-tracking.yml` against alsa-lib's newest published GitHub
   Release (falling back to its newest git tag if no release is published)
   via the GitHub API — independently of the general tracked-path/commit
   diff check, which for alsa-lib (`tracked_paths: []`) only ever reflects
   its default-branch HEAD commit, not a tagged release.
2. When a newer release/tag is found, the `check` job opens a GitHub issue
   (label `upstream/alsa-lib`) *and* emits `alsa_lib_release_detected` /
   `alsa_lib_release_tag` step outputs (because alsa-lib's manifest entry
   sets `run_tests_on_release: true`).
3. A second job, `test_against_new_alsa_lib_release`, runs only when those
   outputs are set: it builds alsa-lib from source at the detected release
   tag, installs it, then configures and runs the native `picoredsp-ioplug`
   CTest suite (`test_ringbuffer`, `test_pcm_worker`, `test_pcm_integration`,
   etc.) against that freshly built alsa-lib — so a real regression is
   caught automatically, not just flagged for a human to remember to check.

> **Important:** alsa-lib version bumps are never auto-merged. The
> automation only opens a tracking issue and runs the existing test suite
> against the new release; upgrading the pinned/expected alsa-lib version
> (e.g. in CI images or documentation) and updating `last_reviewed_tag` /
> `last_reviewed_commit` remain a manual, reviewed decision.

## Review Log

| Date | alsa-lib tag/commit | Decision | Notes |
|------|----------------------|----------|-------|
| 2026-08-15 | `v1.2.16.1` / `a7babcb` | Reviewed | Baseline established. `picoredsp-ioplug` already handles `SND_PCM_IOPLUG_FLAG_BOUNDARY_WA` conditionally (see `picoredsp-ioplug/src/pcm.c`), matching BlueALSA's own conditional-compile pattern for the same alsa-lib feature flag. |
