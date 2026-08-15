# CamillaDSP GUI Upstream Tracking

## Purpose

CamillaDSP GUI (`camillagui-backend` + `camillagui`) is the reference
web front-end for CamillaDSP. It is tracked (LOW priority, see
`docs/ALSA_LIB_TRACKING.md`) because:

- it exercises and documents the full WebSocket + config API surface, often
  ahead of the official CamillaDSP docs;
- breaking GUI ↔ backend changes are an early signal of protocol or schema
  changes that will also affect this project's own WebSocket client;
- UI conventions for pipeline editing, device selection, and volume control
  may inform this project's own config tooling.

It is not a runtime or build dependency of this project.

---

## Repository Details

| Field                        | Value                                                  |
|------------------------------|----------------------------------------------------------|
| Repository (backend)         | <https://github.com/HEnquist/camillagui-backend>          |
| Repository (frontend)        | <https://github.com/HEnquist/camillagui>                  |
| Tracked source paths         | `backend/` (camillagui-backend), `src/` (camillagui)       |
| Last reviewed commit (backend)  | [`4cf1e41`](https://github.com/HEnquist/camillagui-backend/commit/4cf1e4188aebaaf305bf6f462e96fbed4b238808) |
| Last reviewed commit (frontend) | [`948c751`](https://github.com/HEnquist/camillagui/commit/948c751c7974624128d2e8b9f1f066371e7895a7) |
| Last review date             | 2026-08-15                                                |

---

## Topic Categories

Track changes concerning:

| Topic                                                                          |
|-----------------------------------------------------------------------------------|
| WebSocket API calls made by the GUI (new commands, changed arguments)              |
| Config schema assumed by the GUI (pipeline structure, filter types, device fields) |
| Volume / loudness control API                                                     |
| Capture / playback device enumeration                                             |
| Rate and format handling visible in the UI                                        |
| Any breaking change that would require a coordinated update to CamillaDSP itself   |

Ignore purely cosmetic or layout changes with no API or schema implications.

---

## Review Process

Identical to the BlueALSA / CamillaDSP process:

```
camillagui / camillagui-backend release / commit
      ↓
CI notices relevant API/schema change (label: upstream/camillagui)
      ↓
manual review
      ↓
relevant?
  │        │
 no       yes
  │        │
mark     update Rust websocket client / config schema handling
reviewed  + add or update regression test
```

> **Important:** camillagui / camillagui-backend changes are never
> auto-merged or auto-applied. Every detected change requires a manual
> review decision.

---

## Review Log

| Date | Repo | Commit | Topic | Decision | Notes |
|------|------|--------|-------|----------|-------|
| 2026-08-15 | camillagui-backend | `4cf1e41` | Initial tracking baseline | Reviewed | Baseline established. |
| 2026-08-15 | camillagui | `948c751` | Initial tracking baseline | Reviewed | Baseline established. |
