# camilladsp-controller Upstream Tracking

## Purpose

[`camilladsp-controller`](https://github.com/HEnquist/camilladsp-controller)
is the official Python reference controller and WebSocket client for
CamillaDSP. It defines the authoritative command/response API and control
flow that this project's Rust `camilladsp::websocket` client and
`core::state_machine` control loop re-implement natively. It is a design
reference for protocol/behaviour, not a runtime or build dependency.

---

## Repository Details

| Field                | Value                                                       |
|----------------------|--------------------------------------------------------------|
| Repository           | <https://github.com/HEnquist/camilladsp-controller>          |
| Tracked source paths | `controller.py`, `datastructures.py`, `alsa_listener.py`      |
| Last reviewed commit | [`e9fde20`](https://github.com/HEnquist/camilladsp-controller/commit/e9fde2057d5869e6805a965e9c091bbb9a9e9980) |
| Last review date     | 2026-08-15                                                    |

---

## Topic Categories

Track changes concerning:

| Topic                                                            |
|---------------------------------------------------------------------|
| WebSocket command names and arguments                                |
| Response parsing (field names, types, error codes)                   |
| State machine transitions (Idle → Running → Paused → …)              |
| New commands or deprecations                                        |
| Volume / loudness API                                                |
| Config load / reload / validate commands                            |
| `GetConfig` / `SetConfig` schema                                     |
| Any breaking change in the client ↔ server protocol                  |

Ignore changes that are purely documentation or example scripts with no
protocol implications.

---

## Why These Paths

- `controller.py` — the reference control loop this project's
  `core::state_machine::Controller` mirrors (the 200 ms event-queue poll
  interval documented in `src/core/state_machine.rs` is deliberately matched
  to this file's behaviour).
- `datastructures.py` — the reference command/response payload shapes that
  inform `src/camilladsp/websocket.rs`'s parsing (`ProcessingState`,
  `CommandReason`, `StopReason`).
- `alsa_listener.py` — the reference ALSA stream-detection approach that
  this project's `backend::aloop` module is modeled on (HCTL polling for
  stream start/stop/format changes).

---

## Review Process

Identical to the BlueALSA / CamillaDSP process:

```
camilladsp-controller release / commit
      ↓
CI notices relevant API change (label: upstream/camilladsp-controller)
      ↓
manual review
      ↓
relevant?
  │        │
 no       yes
  │        │
mark     update our Rust websocket client
reviewed  + add or update protocol test
```

> **Important:** camilladsp-controller changes are never auto-merged or
> auto-applied. Every detected change requires a manual review decision.

---

## Review Log

| Date | Commit | Topic | Decision | Notes |
|------|--------|-------|----------|-------|
| 2026-08-15 | `e9fde20` | Initial tracking baseline | Reviewed | Baseline established. Most recent tracked change ("Update sample formats for cdsp 4, update readme") aligns with CamillaDSP 4.x sample-format naming already reflected in this project's `core::config::WaveFormat`; no further action required. |
