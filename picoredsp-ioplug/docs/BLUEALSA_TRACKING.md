# BlueALSA Upstream Tracking

## Purpose

BlueALSA serves as an **engineering reference** for the piCoreDSP ioplug.
It is NOT a runtime dependency, a fork base, or a code source.

The piCoreDSP ioplug uses ALSA's public ioplug API (`pcm_ioplug.h`) as its
stable interface — the same interface BlueALSA uses.  We monitor BlueALSA
upstream to learn from improvements to ALSA plugin semantics without
importing any Bluetooth-specific code.

---

## Repository Details

| Field                  | Value                                                   |
|------------------------|---------------------------------------------------------|
| Repository             | <https://github.com/Arkq/bluez-alsa>                    |
| Tracked source files   | `src/bluealsa-pcm.c`, `src/ringbuf.c`, `src/ringbuf.h` |
| Last reviewed commit   | _(not yet reviewed — initial tracking entry)_           |
| Last review date       | _(not yet reviewed)_                                    |
| Fork point commit      | _(original `alsa_cdsp` fork point not recorded)_        |

---

## Topic Categories

The following topics are considered **relevant** for the piCoreDSP ioplug.
Each review pass should check whether BlueALSA has made improvements in
any of these areas.

### Relevant topics

| Topic                              | Notes |
|------------------------------------|-------|
| C11 atomics usage                  | BlueALSA uses `_Atomic` for ring buffer pointers; piCoreDSP follows the same pattern |
| Ringbuffer pointer synchronisation | acquire/release ordering across producer/consumer threads |
| Period boundary handling           | how `hw_ptr` is advanced in steps of `period_size` |
| Buffer boundary handling           | wrap-around at `buffer_size`; boundary flag usage |
| `poll`/`revents` behaviour         | eventfd signalling; `POLLOUT` vs `POLLIN` semantics for playback |
| XRUN detection                     | when to return `-EPIPE` from `pointer()` |
| Pause/resume synchronisation       | `pause()` callback; timer suspension |
| Drain semantics                    | blocking until ring buffer is empty vs. immediate return |
| Thread cancellation                | safe shutdown of worker threads without data loss |
| Signal masking                     | blocking signals in worker threads |
| Delay accounting                   | `delay()` callback; reporting buffered-but-not-played frames |
| alsa-lib compatibility workarounds | workarounds for alsa-lib quirks discovered in the field |

### Explicitly excluded topics

The following BlueALSA functionality must **never** be copied:

| Topic                        | Reason |
|------------------------------|--------|
| D-Bus                        | Bluetooth transport, not needed |
| Bluetooth codecs (A2DP, SCO) | Not applicable |
| ASHA                         | Hearing-aid profile, not applicable |
| Bluetooth volume             | Not applicable |
| BlueALSA control sockets     | Internal Bluetooth API |
| Codec negotiation            | Not applicable |
| Bluetooth compatibility modes | Not applicable |

---

## Review Process

```
New BlueALSA commit
       ↓
CI automation detects relevant changed file (see CI note below)
       ↓
GitHub issue opened automatically
       ↓
Manual review: is the change relevant to any tracked topic?
       ↓
  YES                         NO
   ↓                           ↓
Port concept / apply fix    Mark issue reviewed, close
```

> **Important:** BlueALSA changes are **never** auto-cherry-picked.
> Every change requires a manual review decision.

---

## CI Automation Note

A CI job should be added (Gate 12 / Phase 19) that:

1. Polls the BlueALSA repository for new commits touching the tracked source files.
2. Opens a GitHub issue in this repository when a relevant change is detected.
3. Never auto-merges or auto-applies any changes.

The job should be idempotent (does not re-open already-reviewed issues).

---

## Review Log

| Date | BlueALSA commit | Topic | Decision | Notes |
|------|-----------------|-------|----------|-------|
| _(initial entry)_ | — | — | Initial tracking document created | No review performed yet |

---

## Findings from Initial Engineering Review

The following learnings were captured during the initial piCoreDSP ioplug
design phase (Gate 4 / M4), based on public BlueALSA documentation and the
design intent described in the piCoreDSP roadmap.

### C11 atomics

- Use `_Atomic(uint64_t)` for ring buffer `write_pos` / `read_pos`.
- Producer uses `memory_order_release` on `write_pos` store.
- Consumer uses `memory_order_acquire` on `write_pos` load.
- Consumer uses `memory_order_release` on `read_pos` store.
- Producer uses `memory_order_acquire` on `read_pos` load.
- This guarantees the data written before `write_pos` is updated is visible
  to the consumer after it reads `write_pos`.

### Ringbuffer pointer synchronisation

- Positions are **never wrapped** at capacity; wrapping is done only for
  array index lookup (`pos & mask`).  This avoids ABA problems.
- Capacity **must be a power of two** to allow cheap masking.
- 64-bit monotone counters mean overflow is not a practical concern
  (would take ~13 million years at 192 kHz stereo 32-bit).

### Period boundary handling

- `hw_ptr` is advanced by exactly `period_size` frames per period consumed.
- The worker thread drains `period_size` frames at a time and signals
  the eventfd after each drain.
- ALSA checks `hw_ptr` progress to determine available space; advancing by
  partial periods can confuse some applications.

### Buffer boundary handling

- The ring buffer capacity is a multiple of `buffer_size` (currently 8×)
  to allow the application and the worker to operate without false
  write-stalls during transient bursts.
- When alsa-lib exposes `SND_PCM_IOPLUG_FLAG_BOUNDARY_WA`, the plugin must set
  that flag and return the monotone `hw_ptr` from `pointer()`.
- Falling back to `hw_ptr % buffer_size` is only for older alsa-lib builds that
  lack the boundary-workaround flag.
- Without the boundary-aware path, `hw_ptr == appl_ptr == 0` after wrap-around
  is indistinguishable from a full buffer, so writable space never reappears
  even though the worker has drained audio.

### `poll`/`revents` behaviour

- One `eventfd` (non-blocking) is used as the poll descriptor.
- The worker posts `1` to the eventfd after consuming a period.
- `poll_revents()` consumes the eventfd counter and sets `POLLOUT`
  (writable / space available) so the application knows it can write more.
- The eventfd is drained during `prepare()` to avoid stale events from a
  previous stream.

### XRUN detection

- An XRUN is detected in `pointer()` when `write_pos - read_pos > buffer_size`.
- This means the application filled the ring buffer faster than the
  consumer could drain it.
- The `pointer()` callback returns `-EPIPE` to signal XRUN to alsa-lib.

### Pause/resume synchronisation

- `pause(enable=1)`: set `paused` flag atomically; stop the stream timer.
- `pause(enable=0)`: restart the stream timer from the current position.
- The worker checks the `paused` flag at the top of its loop and spins
  with a 1 ms sleep while paused (does not drain the ring buffer).

### Drain semantics

- `drain()` blocks until `read_avail == 0` (ring buffer empty).
- The null-sink prototype implements this with a polling sleep loop.
- The real data path (Gate 8) will need to ensure the pipe is flushed
  and CamillaDSP has consumed the tail before returning.

### Thread cancellation

- The worker thread is stopped by setting `worker_running = false` and
  then posting to the eventfd to wake it from `nanosleep`.
- The main thread calls `pthread_join` to wait for clean shutdown.
- No `pthread_cancel` is used — cancellation points are avoided entirely.

### Signal masking

- Worker threads should block `SIGPIPE` (pipe write to a closed fd) and
  `SIGINT`/`SIGTERM` to ensure clean shutdown is handled in the main thread.
- To be implemented when the real pipe data path is wired up (Gate 8).

### Delay accounting

- `delay()` returns the number of frames currently in the ring buffer
  (written by the application but not yet consumed by the worker).
- This is an approximation for the null-sink; the real path will need to
  account for frames in the pipe and CamillaDSP's internal buffers.

### alsa-lib compatibility workarounds

- No workarounds identified yet.  To be updated as field testing reveals
  issues.
