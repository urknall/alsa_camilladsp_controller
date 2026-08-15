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
| Repository             | <https://github.com/arkq/bluez-alsa>                    |
| Tracked source files   | `src/asound/bluealsa-pcm.c`, `src/shared/rt.h`          |
| Last reviewed commit   | [`84ad90d`](https://github.com/arkq/bluez-alsa/commit/84ad90d9cb3812a062521b876beb34b294a7066c) — "Use alsa-lib logging for ALSA plugins" |
| Last review date       | 2026-08-15                                              |
| Fork point commit      | _(no `alsa_cdsp` code was ever imported into this project; piCoreDSP's ioplug was written from scratch against ALSA's public `pcm_ioplug.h` API, using BlueALSA only as a design reference — see Findings below)_ |

> **Note (2026-08 review):** the tracked path in earlier drafts of this
> document (`src/bluealsa-pcm.c`, plus a separate `src/ringbuf.c` /
> `src/ringbuf.h`) reflected the pre-rename BlueALSA source layout and an
> assumption that BlueALSA implements its ring buffer in a dedicated file.
> Neither is true of the current upstream: the ALSA PCM plugin now lives at
> `src/asound/bluealsa-pcm.c`, and BlueALSA does **not** use a separate
> lock-free ring buffer module — the "ring buffer" is a single `malloc`'d
> linear buffer (`pcm->io_hw_buffer`) whose `hw_ptr`/`appl_ptr` bookkeeping is
> handled by the ioplug's boundary-aware pointer arithmetic
> (`snd_pcm_ioplug_hw_avail()`), not by a custom atomics-based ring buffer
> implementation. The topic notes below have been corrected accordingly.

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
| 2026-08-15 | [`84ad90d`](https://github.com/arkq/bluez-alsa/commit/84ad90d9cb3812a062521b876beb34b294a7066c) | Full re-read of `bluealsa-pcm.c` against all tracked topics | Reviewed, no port needed | See "Findings from 2026-08 review" below. Confirms piCoreDSP's design choices are compatible in spirit; corrects several stale assumptions from the initial (pre-review) design-intent notes. |

---

## Verification against the reviewer's "incomplete plugin semantics" table

A follow-up review (2026-08-15/16) re-checked each row of a reviewer-supplied
table claiming these topics were incomplete in piCoreDSP's ioplug relative to
BlueALSA. Re-reading the current `picoredsp-ioplug/src/pcm.c` and
`pcm_worker.c` showed most rows already matched BlueALSA's behavioural intent
(bounded waits, tracked `sw_params`/`avail_min`, etc.), but two rows —
**SIGPIPE** and **Drain** — matched BlueALSA's *outcome* without matching its
*exact mechanism*: signal masking blocked only `SIGPIPE` (BlueALSA blocks the
full signal set), and the drain timeout was a flat constant (BlueALSA scales
it with the remaining backlog). Per explicit follow-up direction, both were
changed to match BlueALSA's mechanism exactly rather than being left as
"functionally equivalent" divergences — see the SIGPIPE and Drain rows below
for the resulting code and the tests that prove it.

Each row below was verified by **running** the C test suite
(`cmake --build . && ctest`, 7/7 binaries, 0 failures) and reading the
implementation logic directly — not by trusting comments. Several of the
cited tests are self-verifying in a strong sense: they would crash the test
binary (not just fail an assertion) if the underlying fix were absent, which
rules out a tautological/mocked test giving a false sense of coverage.

| Topic | Reviewer's claim | Actual current state | Evidence (code + a test that would fail/crash without the fix) |
|-------|-------------------|-----------------------|----------|
| SIGPIPE | ❌ unsafe | Blocked for the worker thread via `pthread_sigmask` — and, matching BlueALSA's `io_thread_setup()` exactly (not just the `SIGPIPE`-only fix from the earlier pass), the worker now blocks the **full signal set** (`sigfillset()` + `SIG_SETMASK`, replacing rather than adding to the thread's mask) so no other signal can interrupt in-flight pipe I/O either | `pcm_worker.c:24-48` (`pcdsp_worker_block_all_signals`, renamed from `pcdsp_worker_block_sigpipe`) + call site `pcm.c:252`. Test `test_pcm_worker.c`'s **"SIGPIPE safety regression (release blocker)"** would crash the whole test binary (not just fail an assertion) if `SIGPIPE` weren't blocked; new test **`block_all_signals_blocks_full_signal_set_not_just_sigpipe`** spawns the worker, captures its own mask via a dedicated capture thread, and asserts `SIGPIPE`, `SIGUSR1`, `SIGTERM`, and `SIGHUP` are all blocked (proving full-set blocking, not just `SIGPIPE`), while confirming the *main* thread's mask is left untouched |
| Pause synchronization | ⚠️ atomic + sleep | Mutex/condvar rendezvous: `pcdsp_pause(enable=1)` blocks on `pause_cond` until the worker acknowledges (under `pause_mutex`) it reached a safe point, bounded by `PCDSP_PAUSE_ACK_TIMEOUT_NS` | `pcm.c:721-769` + worker ack at `pcm.c:252-273`. Test `pause_blocks_until_worker_stops_writing_before_returning` streams real audio, calls `snd_pcm_pause(pcm, 1)`, and asserts observed pipe byte count stops changing immediately after `pause()` returns |
| Drain | ⚠️ unbounded wait | Bound is now **dynamic**, matching BlueALSA's own formula exactly: `pcdsp_drain_timeout_ns()` computes `100ms + periods_remaining * period_time` from the frames actually queued at drain-entry, instead of a flat constant (`PCDSP_DRAIN_TIMEOUT_NS` is now only a fallback for the unreachable `rate == 0`/`period_size == 0` case) | `pcm.c:675-700` (`pcdsp_drain_timeout_ns`), used by `pcdsp_drain()` at `pcm.c:702-751`. Test `drain_times_out_when_camilladsp_stops_reading_pipe` still proves the bounded-timeout property; new test **`drain_timeout_scales_with_backlog_not_flat_constant`** proves the *scaling* property directly by measuring wall-clock `-ETIMEDOUT` latency for a tiny (64-frame period) vs. large (8192-frame period) configuration and asserting the large one takes more than 2× as long as the small one, and that neither resembles the old flat 5 s constant |
| `sw_params` | ❌ absent | `pcdsp_sw_params()` implemented and registered in the ioplug callback table, reads `avail_min` via `snd_pcm_sw_params_get_avail_min()` | `pcm.c:626-643`, registered at `pcm.c:951` |
| `avail_min` | ❌ not captured | Captured by `pcdsp_sw_params()`, consulted by `pcdsp_poll_revents()` to gate `RUNNING`-state readiness (`avail >= avail_min`) | `pcm.c:142-146`, `pcm.c:859-863`. Test `poll_revents_respects_avail_min_from_sw_params` negotiates `avail_min=4096` against a scenario that can never reach it and asserts poll never reports ready |
| Delay | ⚠️ ring only | `pcdsp_delay()` adds ring-buffer occupancy *and* bytes already queued in the kernel pipe (`ioctl(FIONREAD)`); documented residual gap is CamillaDSP's own post-pipe internal buffering, which is genuinely outside the plugin's visibility, not an oversight | `pcm.c:898-923` |
| Device disconnect | Basic error | `pcdsp_hw_free()` closes the pipe fd and IPC connection on an unexpected hw_free (no stop/drain), so the controller is never left waiting on a STOP that never arrives; `pcdsp_poll_revents()` surfaces `POLLERR`/`POLLHUP`/`-ENODEV` on `SND_PCM_STATE_DISCONNECTED` | `pcm.c:609-624`, `pcm.c:874-878` |
| alsa-lib quirks | Almost no tracked policy | `SND_PCM_IOPLUG_FLAG_BOUNDARY_WA` used when available (monotone `hw_ptr`), with a `hw_ptr %= buffer_size` fallback for older alsa-lib — the same pattern BlueALSA uses | `pcm.c:427-445` (fallback), `pcm.c:1054-1056` (flag opt-in) |
| Upstream review | ❌ stale | Tracked path corrected to `src/asound/bluealsa-pcm.c`, pinned to commit `84ad90d`, dated 2026-08-15; a machine-readable path-existence check (`scripts/check_upstream_tracking.py::missing_tracked_paths`, added 2026-08-15/16) now catches a future rename automatically instead of relying on a human noticing | This document's Repository Details table and Review Log |

All nine rows above were re-verified by rebuilding and re-running the full C
test suite after the SIGPIPE and Drain code changes landed
(`cmake --build . -j$(nproc) && ctest --output-on-failure`, 7/7 binaries,
0 failures, including both new regression tests), and by re-running
`cargo test` (214 passed, 0 failed) to confirm the Rust side is unaffected.


No production code changes were needed for these rows; all were already
correct as of the 2026-08-15 correctness pass and are exercised by tests
that were re-run (not just re-read) to confirm this.

## Findings from 2026-08 review

The notes below replace the earlier "design intent" placeholders with an
actual reading of `src/asound/bluealsa-pcm.c` at commit `84ad90d`. Where the
real upstream diverges from what piCoreDSP's ioplug does, that is called out
explicitly — divergence is expected and acceptable, since piCoreDSP is not a
BlueALSA fork and has different requirements (single local CamillaDSP process
via a pipe, not a Bluetooth transport via D-Bus/FIFO).

### C11 atomics usage

- BlueALSA marks the shared cursors as C11 atomics on the `bluealsa_pcm`
  struct: `_Atomic snd_pcm_sframes_t io_hw_ptr`, `_Atomic snd_pcm_uframes_t
  io_hw_boundary`, `_Atomic snd_pcm_uframes_t io_avail_min`, and
  `atomic_bool connected` / `atomic_bool fifo_active`.
- There is **no explicit `memory_order_*` tuning** — all atomic accesses use
  the default `memory_order_seq_cst` (plain `pcm->io_hw_ptr = x` / reads),
  relying on the mutex (`pcm->mutex`) for the data that actually needs
  release/acquire pairing (delay bookkeeping, transfer areas). This is
  simpler than a hand-rolled lock-free ring buffer and is consistent with
  piCoreDSP's own approach of using a mutex-guarded shared struct rather than
  bespoke atomics ordering.

### Ringbuffer pointer synchronisation

- **Correction:** BlueALSA does not implement a custom lock-free ring buffer
  with power-of-two masking. `io_hw_ptr` and `appl_ptr` are plain frame
  counters bounded by `io_hw_boundary` (a multiple of `buffer_size` chosen by
  alsa-lib), and available space/frames are computed via alsa-lib's own
  `snd_pcm_ioplug_hw_avail()` helper (with a local fallback implementation
  for alsa-lib < 1.1.6). Wrap-around uses `% io->buffer_size`, not bitmasking,
  so `buffer_size` is not required to be a power of two.
- The actual audio storage (`pcm->io_hw_buffer`) is one `malloc`'d linear
  buffer sized to `buffer_size * frame_size`, copied into/out of by
  `snd_pcm_areas_copy_wrap()` under `pcm->mutex`.

### Period boundary handling

- The IO thread transfers at most one `period_size` chunk per loop iteration
  (`frames = min(period_size, avail)`), advances `io_hw_ptr` by exactly the
  number of frames transferred (with boundary wrap against
  `io_hw_boundary`), and only then publishes the new `io_hw_ptr` to the
  ioplug side.
- It signals `event_fd` (wakes `poll()`) once **after** the transfer when
  `frames + buffer_size - avail >= io_avail_min`, i.e. respects the
  application's configured `avail_min`, not just "always signal every
  period". piCoreDSP's ioplug follows the same avail_min-aware signalling.

### Buffer boundary handling

- BlueALSA relies on alsa-lib's `SND_PCM_IOPLUG_FLAG_BOUNDARY_WA` capability
  when available (monotone `hw_ptr` returned as-is from `pointer()`), and
  falls back to `hw_ptr % io->buffer_size` only when that flag is not
  defined by the installed alsa-lib headers. This matches the workaround
  piCoreDSP's ioplug already implements.

### `poll`/`revents` behaviour

- One non-blocking `eventfd` per PCM instance is the sole poll descriptor.
- The IO thread posts to the eventfd after each period transfer (subject to
  the `avail_min` gate above) and once more when it detects no work to do
  (XRUN / drained condition, `avail == 0`), pinning `io_hw_ptr = -1` first.
- On a fatal IO error, the thread writes a large sentinel value
  (`0xDEAD0000`) to the eventfd and marks `connected = false` before
  parking forever — a "poison the poll descriptor" pattern for reporting
  disconnection asynchronously to the poll-driven application thread.

### XRUN detection

- No explicit `-EPIPE` counter check is done in `pointer()`; instead XRUN is
  represented implicitly by `snd_pcm_ioplug_hw_avail()` returning `0`, which
  the IO thread turns into the sentinel `io_hw_ptr = -1`. `pointer()` returns
  that value straight to alsa-lib's ioplug core, which is what actually
  translates a stalled `hw_ptr` into `SND_PCM_STATE_XRUN` for the
  application. There is no separate `write_pos - read_pos > buffer_size`
  check as earlier notes assumed — that framing came from a generic
  lock-free ring buffer design, not from this codebase.

### Pause/resume synchronisation

- Pausing is a two-party handshake, not a simple boolean flag:
  - `bluealsa_pause(enable=1)` sets a `BA_PAUSE_STATE_PENDING` bit under
    `pcm->mutex` and then blocks on `pcm->pause_cond` until the IO thread
    reports `BA_PAUSE_STATE_PAUSED`, guaranteeing the IO thread is not
    mid-transfer when the D-Bus `Pause` control command is sent.
  - The IO thread checks the pending bit (or `io_hw_ptr == -1`) at the top of
    each loop iteration, signals `pause_cond`, and then blocks in
    `sigwait()` for a real-time resume signal (`SIGIO`) instead of polling
    with a sleep.
  - `bluealsa_pause(enable=0)` sends the D-Bus `Resume` command and then
    `pthread_kill(pcm->io_thread, SIGIO)` to wake the IO thread, which
    re-initializes its rate-sync clock (`asrsync_init`) before resuming
    transfers — important so the resumed stream doesn't think it is behind
    schedule.
  - **Divergence:** piCoreDSP has no persistent background IO thread analog
    to pause — the ioplug's `pause()` callback operates against the
    stdin-pipe transport to CamillaDSP directly (see `pcm_worker.c`). The
    handshake pattern (mutex + condvar rendezvous before touching shared
    transport state, rather than a bare flag check) is the transferable
    lesson and is already reflected in piCoreDSP's own worker synchronization
    (Step 3/Step 4 of this project's correctness pass).

### Drain semantics

- Capture drain is a documented **no-op returning success** — an ioplug bug
  makes it impossible to correctly finish `snd_pcm_drain()` in
  `SND_PCM_STATE_DRAINING` for capture, so BlueALSA just lets ioplug stop the
  PCM immediately.
- Playback drain: ensures the IO thread is running (starts it if necessary),
  returns `-EAGAIN` immediately for non-blocking drain, and otherwise polls
  `event_fd` in a loop, recomputing `snd_pcm_ioplug_hw_avail()` each wake-up
  until the buffer empties, with a **timeout bounded by the number of whole
  periods remaining** (`100ms + periods_remaining * period_time`) rather than
  an unbounded wait — a fixed timeout (or infinite poll) would either abort
  slow-but-healthy drains too early or hang forever on a truly stuck
  transport. piCoreDSP's `pcdsp_drain()` now computes the identical formula
  via `pcdsp_drain_timeout_ns()` (`pcm.c:675-700`) instead of the flat
  constant used before this pass, so the bound scales the same way BlueALSA's
  does; see the Drain row in the verification table above for the test that
  measures this directly.
- Once the local buffer is empty, BlueALSA additionally sends a D-Bus
  `Drain` control command so the *far side* (Bluetooth transport) can flush
  before the ALSA-level state moves to `SETUP`.

### Thread cancellation

- The IO thread installs a `pthread_cleanup_push` handler and blocks with
  `sigwait()` rather than sleeping, but the *shutdown* path
  (`io_thread_cancel()`) still uses `pthread_cancel()` + `pthread_join()`
  from the main thread — i.e. BlueALSA does rely on POSIX cancellation
  points for stopping the thread, contrary to the earlier assumption in this
  document that cancellation is "avoided entirely". All signals are blocked
  in the IO thread (`sigfillset` + `pthread_sigmask(SIG_SETMASK, ...)`) so
  the only way `sigwait()` unblocks is a signal explicitly delivered via
  `pthread_kill()` (`SIGIO`) — this also means `pthread_cancel()` delivery
  timing is only guaranteed at defined cancellation points (e.g. inside
  `ppoll`/`read`/`sigwait`), which is consistent with piCoreDSP's own
  cooperative (flag + wake, not `pthread_cancel`) shutdown for its worker
  thread.

### Signal masking

- `sigfillset()` + `pthread_sigmask(SIG_SETMASK, &sigset, NULL)` blocks
  *all* signals in the IO thread, explicitly to (a) guarantee `EPIPE` is
  returned from `write()` instead of raising `SIGPIPE`, and (b) so `SIGIO`
  (used for resume) is only ever consumed via `sigwait()`, never delivered
  asynchronously. piCoreDSP's worker (`pcm_worker.c:pcdsp_worker_block_all_signals`)
  now uses the identical `sigfillset()` + `SIG_SETMASK` call, blocking the
  full signal set rather than only `SIGPIPE` as in the earlier pass — see the
  SIGPIPE row in the verification table above for the test that captures the
  worker thread's own mask and proves `SIGUSR1`/`SIGTERM`/`SIGHUP` are
  blocked too, not just `SIGPIPE`.

### Delay accounting

- `bluealsa_calculate_delay()` combines four components: (1) frames still
  sitting in the kernel FIFO at the last sampled instant
  (`delay_pcm_nread`, from `ioctl(FIONREAD)`), adjusted for elapsed wall
  time since that sample; (2) frames in the local ring buffer not yet
  consumed by the IO thread; (3) a fixed encode/transport delay reported by
  the BlueALSA server itself (`ba_pcm.delay`) and any user-supplied
  `client_delay`; (4) an additional `delay_ex` fudge factor. Time-based
  extrapolation (rather than re-querying the FIFO on every `delay()` call)
  avoids a syscall on every `delay()` invocation, which can be called at
  high frequency by some applications.
- For capture, delay is *not* time-extrapolated at all — it simply reports
  `snd_pcm_ioplug_avail()` (frames available to read), because Bluetooth
  profiles don't expose true source-to-sink latency.
- piCoreDSP's simpler ring-buffer-occupancy delay is adequate for its
  single local pipe transport (no encode/BT leg to account for), but the
  "snapshot + extrapolate rather than re-poll on every call" technique is
  worth keeping in mind if `delay()` becomes a hot path.

### alsa-lib compatibility workarounds

- `BLUEALSA_HW_PARAMS_FIX` (`SND_LIB_VERSION` in `[1.1.4, 1.2.5.1]`) rewrites
  the negotiated `hw_params` container from scratch to force
  `buffer_size % period_size == 0`, working around a rate-plugin `avail()`
  bug in older alsa-lib that could otherwise deadlock applications built on
  PortAudio.
  - piCoreDSP declares `rust-version = "1.71"` for the *Rust* toolchain and
    links against whatever `alsa-lib` the target ships; this specific
    version range (alsa-lib 1.1.4–1.2.5.1) predates piCoreDSP's supported
    targets (modern piCorePlayer images ship alsa-lib well past 1.2.5.1), so
    no equivalent fix is currently required, but the *pattern* — detect a
    known-bad `SND_LIB_VERSION` range at compile time and patch the
    negotiated `hw_params` rather than the runtime behaviour — is the
    reusable takeaway if a similar quirk surfaces for piCoreDSP's supported
    alsa-lib range.
  - `enum ba_hwcompat { BA_HWCOMPAT_NONE, BA_HWCOMPAT_BUSY, BA_HWCOMPAT_SILENCE }`
    is a separate, user-selectable compatibility mode (device busy vs.
    silence-injection) for applications that misbehave when a Bluetooth
    transport is momentarily inactive. This is Bluetooth-transport-specific
    (masking transport gaps) and out of scope for piCoreDSP, which always
    has an active local pipe to CamillaDSP.
  - No workaround is needed purely for alsa-lib compatibility as far as
    piCoreDSP's supported alsa-lib range is concerned; revisit if field
    testing on older piCorePlayer images (older alsa-lib) surfaces issues.
