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

A new alsa-lib release should trigger the plugin test suite automatically,
even if no source change is required in this repository, so that a real
regression (not just a stale review-log entry) is caught. This is
implemented as a scheduled job in
`.github/workflows/upstream-tracking.yml`: on detecting a newer alsa-lib
release tag than `last_reviewed_tag` in `docs/upstream-tracking.yml`, it
opens a GitHub issue (label `upstream/alsa-lib`) prompting a manual review
and a run of the full `native_c_tests` / `asan` / `ubsan` / `tsan` CI jobs
against the new alsa-lib package version, if available in the CI image.

> **Important:** alsa-lib version bumps are never auto-merged. The
> automation only opens a tracking issue; upgrading the pinned/expected
> alsa-lib version (e.g. in CI images or documentation) is a manual,
> reviewed decision.

## Review Log

| Date | alsa-lib tag/commit | Decision | Notes |
|------|----------------------|----------|-------|
| 2026-08-15 | `v1.2.16.1` / `a7babcb` | Reviewed | Baseline established. `picoredsp-ioplug` already handles `SND_PCM_IOPLUG_FLAG_BOUNDARY_WA` conditionally (see `picoredsp-ioplug/src/pcm.c`), matching BlueALSA's own conditional-compile pattern for the same alsa-lib feature flag. |
