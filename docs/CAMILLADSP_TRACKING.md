# CamillaDSP Upstream Tracking

## Purpose

CamillaDSP is a **direct runtime dependency**: this project spawns it,
drives it over its WebSocket control API, and depends on its config schema,
process lifecycle semantics, and (for the ioplug backend) its stdin/pipe
audio transport. Unlike BlueALSA (an engineering *reference*), CamillaDSP
changes can directly break this controller, so it gets a HIGH maintenance
priority (see Phase 20 / `docs/ALSA_LIB_TRACKING.md` for the full priority
list).

---

## Repository Details

| Field                | Value                                                    |
|----------------------|-----------------------------------------------------------|
| Repository           | <https://github.com/HEnquist/camilladsp>                 |
| Tracked source paths | `src/bin.rs`, `src/socketserver.rs`, `src/config/`        |
| Last reviewed tag    | `v4.1.3`                                                  |
| Last reviewed commit | [`05e9cfc`](https://github.com/HEnquist/camilladsp/commit/05e9cfcdf43c0dfe078ed3feb8af4c8bd701fd74) |
| Last review date     | 2026-08-15                                                |

---

## Topic Categories

Track changes concerning:

| Topic                                                          |
|------------------------------------------------------------------|
| WebSocket API (command names, response format, state machine)    |
| Config file schema (pipeline, filters, devices, resampler)       |
| Process lifecycle (startup, shutdown, error codes, exit behaviour) |
| stdin/pipe audio transport                                       |
| Rate/format negotiation                                          |
| Capture/playback device handling                                 |
| Volume / loudness commands                                       |
| Signal path (capture → processing → playback)                    |
| Breaking changes in any of the above                              |

Ignore changes that are purely internal optimisations with no externally
observable effect (e.g. DSP kernel performance tuning, internal refactors
that don't touch `socketserver.rs`'s command surface).

---

## Why These Paths

- `src/bin.rs` — process entry point: command-line flags, startup sequence,
  the `"Playback error: ..."` early-exit marker this project's
  `core::recovery` classification logic depends on (see Step 5/Step 6 of
  the correctness follow-up plan).
- `src/socketserver.rs` — the WebSocket command/response surface this
  project's `camilladsp::websocket` client re-implements from scratch in
  Rust (command names, `ProcessingState`, error payloads).
- `src/config/` — the YAML config schema (`devices`, `filters`, `mixers`,
  `pipeline`) that `core::config` / `core::adaptation` read, validate, and
  runtime-patch per backend.

---

## Review Process

Identical to the BlueALSA process (see
`picoredsp-ioplug/docs/BLUEALSA_TRACKING.md`):

```
CamillaDSP release / commit
      ↓
CI notices tracked paths changed (see .github/workflows/upstream-tracking.yml)
      ↓
GitHub issue opened (label: upstream/camilladsp)
      ↓
manual review
      ↓
relevant?
  │        │
 no       yes
  │        │
mark     update websocket client / config schema / lifecycle handling
reviewed  + add or update regression test
```

> **Important:** CamillaDSP changes are never auto-merged or auto-applied.
> Every detected change requires a manual review decision, recorded in the
> Review Log below.

---

## Review Log

| Date | CamillaDSP commit/tag | Topic | Decision | Notes |
|------|------------------------|-------|----------|-------|
| 2026-08-15 | `05e9cfc` / `v4.1.3` | Initial tracking baseline | Reviewed | Baseline established: `v4.1.3` is the version this project's WebSocket client (`src/camilladsp/websocket.rs`) and recovery classification (`src/core/recovery.rs`) were last validated against. Notable recent upstream change: `v4.1.3` adjusted internal ringbuffer sizing when resampling is active (PR #456) — an internal DSP-path change with no observable WebSocket/config/lifecycle impact, so no action required here. |
