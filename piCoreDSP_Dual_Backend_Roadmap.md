# piCoreDSP Dual-Backend Architecture Roadmap

## 1. Project goal

The long-term goal is to evolve piCoreDSP from a single `snd-aloop` based architecture into a **backend-neutral Rust control platform** supporting two independent audio transport implementations:

```text
                           piCoreDSP
                              │
                    Rust Control Core
                              │
             ┌────────────────┴────────────────┐
             │                                 │
             ▼                                 ▼
     Backend A: Aloop                 Backend B: ioplug
     "robust / standard"              "direct / low latency"
             │                                 │
         snd-aloop                     custom ALSA ioplug
             │                                 │
     CamillaDSP ALSA                   CamillaDSP stdin
          capture                         capture
             │                                 │
             └────────────────┬────────────────┘
                              ▼
                         CamillaDSP
                              │
                              ▼
                             DAC
```

The existing Rust controller remains the common control plane. The new ioplug is **an additional transport backend, not a replacement for the Rust controller**.

The two implementations should eventually be selectable at installation or configuration time:

```text
backend = aloop
```

or:

```text
backend = ioplug
```

Only one backend should be active at a time.

---

## 2. Design principles

The project should follow several strict principles from the beginning.

### Keep the Rust controller

The Rust daemon remains responsible for:

```text
active config selection
baseline YAML loading
runtime config adaptation
CamillaGUI integration
persistent state
validation
logging
retry / recovery
CamillaDSP supervision
startup behaviour
error handling
```

The new plugin must not duplicate these responsibilities.

### Keep PCM out of Rust

Rust should remain outside the actual sample path.

Do **not** build:

```text
Player
  ↓
C plugin
  ↓
Rust
  ↓
CamillaDSP
```

Instead:

```text
CONTROL:

C plugin ─────────► Rust controller


AUDIO:

C plugin ════════► CamillaDSP
```

This preserves the current separation between control plane and data plane.

### Do not fork BlueALSA as a permanent dependency

BlueALSA should serve as:

```text
reference implementation
bug knowledge source
ioplug design reference
test inspiration
```

but not as:

```text
runtime dependency
source dependency
permanent fork base
```

The stable API boundary should be **ALSA's public ioplug API**.

### Preserve the current backend

The `snd-aloop` solution should remain fully functional throughout development.

At no stage should plugin development require breaking the production backend.

---

## 3. Target software architecture

A useful final Rust layout would look conceptually like this:

```text
picoredsp-controller
│
├── core/
│   ├── state_machine.rs
│   ├── config.rs
│   ├── adaptation.rs
│   ├── persistence.rs
│   ├── errors.rs
│   └── logging.rs
│
├── backend/
│   ├── mod.rs
│   ├── aloop.rs
│   └── ioplug.rs
│
├── camilladsp/
│   ├── websocket.rs
│   ├── supervisor.rs
│   ├── alsa_capture.rs
│   └── stdin_capture.rs
│
└── ipc/
    ├── protocol.rs
    └── unix_socket.rs
```

The important architectural boundary is:

```text
                    StreamEvent
                         │
                         ▼
                 Rust Control Core
                         │
                         ▼
                Runtime Configuration
```

The core should not care whether the stream parameters came from:

```text
snd-aloop HCTL
```

or:

```text
ALSA hw_params from ioplug
```

---

## 4. Phase 0 — Freeze the current production baseline

Before changing architecture, establish the current `aloop` implementation as the reference.

The current production path becomes:

```text
backend = aloop
status  = stable
```

Document its behaviour and acceptance tests, including:

```text
idle reboot
first playback
44.1 → 48 → 96 kHz
format changes
stop/start
GUI Apply and Save
active-config selection
pcp backup
reboot persistence
controller restart
CamillaDSP restart
transient WebSocket failure
```

### Gate 0

No backend refactoring begins until the current behaviour is reproducible through a defined acceptance suite.

This gives us a golden reference against which all later changes are tested.

---

## 5. Phase 1 — Refactor Rust into backend-neutral core logic

This is the most important preparatory step.

Do **not** write the new plugin first.

First separate:

```text
"how a stream was detected"
```

from:

```text
"what piCoreDSP does with that stream"
```

A common representation could be:

```rust
pub struct StreamParams {
    pub rate: u32,
    pub format: SampleFormat,
    pub channels: u32,
}

pub enum StreamEvent {
    Started(StreamParams),
    Changed(StreamParams),
    Stopped,
}
```

The current ALSA/HCTL code should become one producer of these events.

Conceptually:

```rust
trait StreamBackend {
    fn next_event(&mut self) -> Result<StreamEvent>;
}
```

The existing implementation becomes:

```text
AloopBackend
    ↓
reads HCTL
    ↓
produces StreamEvent
```

The new implementation will later become:

```text
IoplugBackend
    ↓
reads IPC
    ↓
produces StreamEvent
```

### Gate 1

After refactoring:

```text
backend=aloop
```

must behave identically to today's implementation.

No new functionality yet.

This is a pure architectural refactor.

---

## 6. Phase 2 — Separate stream detection from audio transport

There are actually two dimensions involved.

The current architecture uses:

```text
Detector:
    snd-aloop HCTL

Transport:
    CamillaDSP ALSA capture
```

The new architecture uses:

```text
Detector:
    plugin IPC

Transport:
    CamillaDSP stdin
```

Model those explicitly.

For example:

```text
Backend profile: aloop
----------------------
detector  = AloopHctl
transport = AlsaCapture


Backend profile: ioplug
-----------------------
detector  = IoplugIpc
transport = StdinPipe
```

This avoids filling the common core with backend-specific branching everywhere.

Instead each backend supplies its capabilities and lifecycle.

---

## 7. Phase 3 — Define piCoreDSP-managed vs user-managed config fields

This deserves an explicit policy.

A custom user's DSP design should remain backend independent.

### User-owned configuration

Typically:

```text
filters
mixers
processors
pipeline
playback device
labels
gains
delays
crossovers
FIR coefficients
chunksize
queuelimit
```

### Runtime/backend-managed fields

Typically:

```text
devices.samplerate
devices.capture.type
devices.capture.device
devices.capture.format
devices.capture.channels
devices.capture.stop_on_inactive
possibly enable_rate_adjust
```

The same persistent DSP baseline should ideally work with either backend.

Example persistent conceptual configuration:

```yaml
devices:
  samplerate: 44100

  playback:
    type: Alsa
    channels: 2
    device: plughw:CARD=mydac,DEV=0

filters:
  ...
mixers:
  ...
pipeline:
  ...
```

At runtime, `aloop` could generate:

```yaml
capture:
  type: Alsa
  channels: 2
  device: hw:Loopback,0,0
  stop_on_inactive: true
```

while `ioplug` generates:

```yaml
capture:
  type: Stdin
  channels: 2
  format: S24_4_LE
```

The actual runtime sample rate comes from the current stream.

This would make DSP configs far more portable between the two architectures.

---

## 8. Phase 4 — Build a modern standalone ALSA ioplug

Only after the Rust core is backend-neutral should plugin development start.

The new plugin should **not** be a continuation of the old `alsa_cdsp` source tree.

Suggested structure:

```text
picoredsp-ioplug/
│
├── src/
│   ├── pcm.c
│   ├── ringbuffer.c
│   ├── ringbuffer.h
│   ├── ipc.c
│   ├── ipc.h
│   ├── timing.c
│   └── format.c
│
├── tests/
│
├── docs/
│   └── BLUEALSA_TRACKING.md
│
└── CMakeLists.txt / Makefile
```

The first plugin prototype should **not start CamillaDSP**.

Initially it should only prove that it can:

```text
load as ALSA PCM
negotiate hw_params
receive PCM
maintain correct hw_ptr
handle periods correctly
report poll state
handle XRUN
pause/resume
drain/drop
close cleanly
```

This keeps the first milestone focused exclusively on ALSA correctness.

---

## 9. Phase 5 — Use modern BlueALSA as engineering reference

Review the current BlueALSA PCM implementation and its historical changes since the original `alsa_cdsp` fork point.

Relevant topics include:

```text
C11 atomics
ringbuffer pointer synchronization
period boundary handling
buffer boundary handling
poll/revents behaviour
XRUN detection
pause/resume synchronization
drain semantics
thread cancellation
signal masking
delay accounting
alsa-lib compatibility workarounds
```

Do not copy:

```text
D-Bus
Bluetooth codecs
A2DP
SCO
ASHA
Bluetooth volume
BlueALSA control sockets
codec negotiation
Bluetooth compatibility modes
```

The result should be a **small CamillaDSP-specific ioplug**, not a de-Bluetoothified BlueALSA fork.

---

## 10. Phase 6 — Define the plugin ↔ Rust IPC protocol

The IPC protocol should remain extremely small.

Recommended transport:

```text
AF_UNIX socket
```

A message-oriented Unix socket is attractive because the protocol is local-only and no networking configuration is required.

Protocol versioning should exist from day one.

Example:

```text
HELLO
protocol = 1

START
rate     = 96000
format   = S24_LE
channels = 2

STOP

READY

ERROR
code = ...
```

Internally this could use a compact binary representation rather than textual parsing.

For example:

```rust
enum PluginMessage {
    Hello { version: u16 },

    Start {
        rate: u32,
        format: u32,
        channels: u16,
    },

    Stop,
}
```

The protocol must explicitly define:

```text
endianness
version negotiation
unknown messages
disconnect behaviour
timeouts
maximum message length
reconnect behaviour
controller unavailable behaviour
```

---

## 11. Phase 7 — Implement the START / READY handshake

This is one of the major architectural advantages over `snd-aloop`.

Expected sequence:

```text
Audio application
      │
      ▼
ALSA hw_params negotiated
      │
      ▼
ioplug knows exact:
rate / format / channels
      │
      ▼
START(params)
      │
      ▼
Rust controller
      │
      ├── read active baseline
      ├── validate it
      ├── adapt runtime configuration
      ├── prepare CamillaDSP
      │
      ▼
READY
      │
      ▼
ioplug starts PCM transfer
```

This eliminates the observational stage:

```text
ALSA HCTL event
→ debounce
→ snapshot
→ inference
```

because the negotiated parameters are known before streaming starts.

The important invariant becomes:

> PCM must not be released toward CamillaDSP until the controller has acknowledged that the matching runtime configuration is ready.

---

## 12. Phase 8 — Implement direct stdin PCM transport

This is the most delicate new part.

The desired data path is:

```text
Player
  ↓
ALSA ioplug
  ↓
pipe
  ↓
CamillaDSP stdin
  ↓
DSP
  ↓
DAC
```

Rust should supervise this without processing PCM.

A clean Linux implementation would be:

```text
Rust:
    pipe()

Rust:
    spawn CamillaDSP
    stdin = pipe read fd

Rust:
    pass write fd to plugin over Unix socket

Plugin:
    writes PCM directly into fd
```

Unix-domain FD passing with `SCM_RIGHTS` is a strong candidate here.

Then the actual path remains:

```text
Plugin ───── kernel pipe ─────► CamillaDSP
```

not:

```text
Plugin → Rust userspace → CamillaDSP
```

That distinction should be treated as an architectural invariant.

---

## 13. Phase 9 — Backend-specific CamillaDSP lifecycle

The two backends do not need identical CamillaDSP process semantics.

### Aloop backend

Can retain the existing model:

```text
CamillaDSP
--wait
--no_config

source starts
→ Rust SetConfig
```

### ioplug backend

May use a per-stream stdin process lifecycle:

```text
START(params)
→ Rust creates pipe
→ Rust adapts config
→ Rust starts CamillaDSP with stdin
→ READY
→ PCM
→ stream ends
→ plugin closes pipe
→ EOF
→ CamillaDSP shuts down
```

Trying to force both backends into exactly the same process model would likely create unnecessary complexity.

The common abstraction should instead describe intent:

```text
prepare stream
start processing
stop processing
recover processing
```

Each backend is free to implement that correctly.

---

## 14. Phase 10 — Preserve and reuse Rust recovery logic

The ioplug backend should reuse the mature concepts already present in the controller:

```text
validation failures
transient failures
retry/backoff
startup timeout
process failure handling
configuration fingerprint changes
logging
state transitions
shutdown
```

The plugin should not implement policy such as:

```text
retry five times
reload config
choose Bypass
update active_config.yml
```

That remains Rust territory.

The C component should know as little policy as possible.

Ideal plugin philosophy:

```text
"I am an ALSA PCM endpoint.

These are my negotiated parameters.

Here are the PCM samples.

Tell me when the consumer is ready."
```

Nothing more.

---

## 15. Phase 11 — Plugin failure model

Before real hardware testing, explicitly define behaviour for every important failure.

### Rust controller absent

```text
ALSA open
→ plugin cannot connect
→ fail cleanly with meaningful ALSA error
```

Do not silently discard samples.

### Invalid DSP config

```text
START
→ Rust validation fails
→ ERROR_CONFIG
→ ALSA start fails
```

### CamillaDSP cannot open DAC

```text
START
→ spawn
→ CamillaDSP fails
→ Rust detects failure
→ ERROR_PLAYBACK_DEVICE
```

### CamillaDSP exits mid-stream

```text
pipe breaks
→ plugin receives EPIPE
→ terminate ALSA stream cleanly
→ Rust records failure
```

### Plugin process/application disappears

```text
control socket closes
+
PCM fd closes
→ Rust cleans up CamillaDSP
```

### Rust daemon restarts

The initial ioplug implementation can simply fail the active stream.

Later versions may implement reconnect if worthwhile.

Do not make automatic reconnect a v1 requirement.

---

## 16. Phase 12 — Build serious plugin tests

A C plugin in the audio application's address space needs stronger tests than an external controller.

Minimum unit/integration coverage:

```text
open/close
hw_params
unsupported format
unsupported channels
44.1 kHz
48 kHz
88.2 kHz
96 kHz
176.4 kHz
192 kHz

period wrap
buffer wrap
buffer size not divisible by period
partial write
EINTR
EPIPE

poll descriptors
poll revents
pause
resume
drain
drop
XRUN
rapid open/close
rapid format change

controller unavailable
controller timeout
invalid READY
protocol mismatch
socket disconnect

CamillaDSP early exit
CamillaDSP delayed startup
DAC unavailable
```

CI should additionally run:

```text
ASAN
UBSAN
TSAN where practical
clang warnings
gcc warnings
static analysis
```

Prefer:

```text
-Wall
-Wextra
-Wpedantic
-Werror
```

for supported compiler configurations.

---

## 17. Phase 13 — Cross-backend Rust tests

The Rust tests should stop assuming `snd-aloop` is the only event source.

The same behavioural suite should be run against abstract events.

Example:

```text
Started(44100, S16, 2)
→ correct runtime config

Changed(48000, S24, 2)
→ correct restart/adaptation

Stopped
→ correct idle state
```

Then repeat using both adapters:

```text
Aloop event source
Ioplug IPC event source
```

This lets the majority of controller behaviour be tested once.

---

## 18. Phase 14 — Build an A/B benchmark framework

The existence of two backends creates a valuable opportunity:

**measure them instead of arguing theoretically.**

Same:

```text
Raspberry Pi
piCorePlayer
CamillaDSP version
DAC
DSP configuration
track
sample rate
chunksize
queuelimit
```

Change only:

```text
backend=aloop
```

versus:

```text
backend=ioplug
```

Measure at least:

| Metric | Aloop | ioplug |
|---|---:|---:|
| playback start latency | | |
| 44.1→48 transition time | | |
| 48→96 transition time | | |
| stop latency | | |
| PCM transport latency | | |
| total end-to-end latency | | |
| CPU usage | | |
| context switches | | |
| controller RSS | | |
| plugin overhead | | |
| XRUN count | | |
| 24h stability | | |
| 7-day stability | | |
| recovery after DAC error | | |

End-to-end latency should ideally be measured externally where possible rather than inferred exclusively from buffer sizes.

---

## 19. Phase 15 — Audio integrity testing

The plugin should not merely "produce sound".

Verify actual PCM integrity.

Useful tests include:

```text
known PCM pattern
→ plugin
→ capture output/reference
→ binary comparison
```

where possible.

Test:

```text
S16_LE
S24_3LE
S24_4LE
S32_LE
F32_LE
```

and all intended rates.

Verify that there is no accidental:

```text
resampling
channel swap
byte-order error
24-bit alignment error
truncation
gain modification
padding corruption
```

The goal is to establish:

> For supported input formats, the ioplug transport is bit-transparent before CamillaDSP processing.

---

## 20. Phase 16 — Latency tuning

Only after correctness is established should buffer sizes be optimized.

Do not start development by chasing minimal latency.

Order:

```text
correctness
    ↓
stability
    ↓
measurement
    ↓
latency optimization
```

Candidates for tuning:

```text
ALSA period size
ALSA buffer size
plugin ringbuffer depth
pipe size
CamillaDSP chunksize
CamillaDSP queuelimit
DAC period/buffer parameters
```

Results should be measured separately for:

```text
44.1 kHz
48 kHz
96 kHz
192 kHz
```

because one fixed frame count corresponds to very different time durations at different rates.

---

## 21. Phase 17 — Installer integration

Once the ioplug passes development tests, the installer can install both backends.

Example:

```text
/usr/local/bin/picoredsp-controller

/usr/local/lib/alsa-lib/
    libasound_module_pcm_picoredsp.so
```

User-facing configuration:

```text
piCoreDSP backend:

[ ] snd-aloop (recommended / stable)
[ ] direct ioplug (experimental)
```

Initially:

```text
default = aloop
```

The installer should generate the ALSA configuration appropriate for the selected backend.

### Aloop

```text
pcm.picoredsp
    ↓
snd-aloop
```

### ioplug

```text
pcm.picoredsp
    ↓
libasound_module_pcm_picoredsp.so
```

Switching backend should require an explicit restart/reboot.

No dynamic in-stream backend switching.

---

## 22. Phase 18 — Configuration migration

Existing configs must continue to work.

The controller should normalize the selected baseline into a runtime config appropriate for the backend.

That means users should **not need two copies** such as:

```text
MySpeakers-aloop.yml
MySpeakers-ioplug.yml
```

Instead:

```text
MySpeakers.yml
```

should ideally work with both.

This is an important usability goal.

The runtime transport details are piCoreDSP implementation details and should not contaminate every custom DSP configuration.

---

## 23. Phase 19 — BlueALSA upstream monitoring

BlueALSA remains a reference upstream.

Create:

```text
docs/BLUEALSA_TRACKING.md
```

or machine-readable:

```text
docs/bluealsa-upstream.yml
```

containing:

```text
repository
tracked source files
last reviewed commit
review date
relevant topic categories
```

Track changes concerning:

```text
ioplug
ringbuffer
atomics
hw_ptr
period handling
poll/revents
XRUN
delay
pause
drain
thread safety
alsa-lib compatibility
```

Ignore Bluetooth-specific changes.

Automation should detect new relevant changes, but **never automatically merge them**.

Preferred process:

```text
BlueALSA update
      ↓
CI notices tracked file changed
      ↓
GitHub issue
      ↓
manual review
      ↓
relevant?
  │        │
 no       yes
  │        │
mark     port concept/fix
reviewed  + add regression test
```

---

## 24. Phase 20 — Monitor alsa-lib separately

BlueALSA is not the real API provider.

The more important dependency is:

```text
alsa-lib
```

because both BlueALSA and our plugin use its ioplug interface.

Maintenance priorities should therefore be:

```text
HIGH
    alsa-lib
    CamillaDSP
    piCorePlayer

MEDIUM
    Linux ALSA
    BlueALSA reference changes

LOW
    unrelated BlueALSA Bluetooth functionality
```

A new alsa-lib release should trigger the plugin test suite even if no source change is required.

---

## 25. Phase 21 — Experimental release

First public ioplug release:

```text
backend = ioplug
status  = experimental
```

while:

```text
backend = aloop
status  = recommended
```

Do not promote the new backend based only on short functional testing.

The experimental period should include:

```text
multiple Raspberry Pi generations
multiple DACs
long-running playback
frequent sample-rate changes
AirPlay
Bluetooth
Squeezelite
GUI editing
reboots
controller restarts
CamillaDSP failures
```

---

## 26. Phase 22 — Production promotion gate

The ioplug backend becomes production-ready only if it demonstrates:

```text
no PCM corruption
no significant crash regressions
no unexplained XRUN regressions
correct format handling
correct sample-rate handling
reliable pause/stop/start
reliable CamillaDSP cleanup
reliable GUI persistence
reliable reboot behaviour
clean controller failure handling
long-duration stability
```

And ideally shows a measurable benefit in at least one meaningful area:

```text
lower latency
faster rate switching
simpler runtime architecture
lower CPU/context-switch cost
better determinism
```

Otherwise there is little reason to make it the default.

---

## 27. Possible long-term product states

There are three valid outcomes.

### Outcome A — Aloop remains default

```text
aloop   = stable/default
ioplug  = optional low-latency backend
```

This is perfectly acceptable.

### Outcome B — ioplug becomes default

```text
ioplug  = default
aloop   = compatibility/fallback
```

This would make sense if the plugin proves equally robust and measurably simpler/faster.

### Outcome C — Both remain first-class

```text
aloop:
    maximum isolation / conventional ALSA architecture

ioplug:
    direct path / minimum latency
```

Users choose depending on their priorities.

This may actually be the best final state.

---

## 28. Things we should explicitly **not** do

Avoid these architectural traps:

```text
Do not remove the working aloop backend early.

Do not pass PCM through the Rust daemon.

Do not copy the complete current bluealsa-pcm.c.

Do not make BlueALSA a runtime dependency.

Do not duplicate config/persistence logic inside the C plugin.

Do not let the C plugin decide policy.

Do not automatically cherry-pick BlueALSA changes.

Do not auto-switch between aloop and ioplug while audio is running.

Do not maintain separate DSP configs for each backend unless unavoidable.

Do not optimize latency before correctness and stability.
```

---

## 29. Suggested milestone sequence

A practical development sequence would be:

```text
M0  Freeze current aloop baseline
 │
 ▼
M1  Refactor Rust into backend-neutral core
 │
 ▼
M2  Reimplement current aloop as backend module
 │
 ▼
M3  Establish identical behaviour / regression tests
 │
 ▼
M4  Build standalone modern ALSA ioplug
 │
 ▼
M5  Validate ALSA ringbuffer / poll / XRUN semantics
 │
 ▼
M6  Implement versioned plugin ↔ Rust IPC
 │
 ▼
M7  Implement START / READY handshake
 │
 ▼
M8  Implement stdin pipe + FD handoff
 │
 ▼
M9  Add Rust stdin CamillaDSP supervisor
 │
 ▼
M10 Run complete ioplug functional suite
 │
 ▼
M11 Run audio-integrity tests
 │
 ▼
M12 Run A/B latency and performance benchmarks
 │
 ▼
M13 Integrate both backends into installer
 │
 ▼
M14 Experimental real-hardware release
 │
 ▼
M15 Long-term field testing
 │
 ▼
M16 Decide default backend
```

This ordering deliberately minimizes risk.

---

## 30. Final target architecture

The final design would combine the strongest ideas from all previous approaches:

```text
                    ┌───────────────────────────┐
                    │     piCoreDSP Rust Core   │
                    │                           │
                    │ config / persistence      │
                    │ CamillaGUI integration    │
                    │ validation                │
                    │ recovery                  │
                    │ supervision               │
                    └─────────────┬─────────────┘
                                  │
                   backend-neutral StreamEvent
                                  │
              ┌───────────────────┴───────────────────┐
              │                                       │
              ▼                                       ▼
      ┌────────────────┐                     ┌─────────────────┐
      │  ALOOP BACKEND │                     │ IP﻿LUG BACKEND   │
      │                │                     │                 │
      │ snd-aloop      │                     │ custom ALSA PCM │
      │ HCTL           │                     │ exact hw_params │
      │ ALSA capture   │                     │ control IPC     │
      └───────┬────────┘                     └────────┬────────┘
              │                                       │
              │ PCM                                   │ PCM
              ▼                                       ▼
        CamillaDSP ALSA                       CamillaDSP stdin
              │                                       │
              └───────────────────┬───────────────────┘
                                  ▼
                                 DSP
                                  │
                                  ▼
                                 DAC
```

Conceptually, this gives piCoreDSP:

> **one control architecture, one persistence model, one GUI model, one DSP configuration model — but two interchangeable audio transport architectures.**

The existing `snd-aloop` backend provides the conservative, isolated and already proven path.

The future ioplug backend can provide the direct, exact-parameter, potentially lower-latency path.

Most importantly, **development of the second architecture does not invalidate the first one**. It becomes an additional capability of the same Rust-based platform rather than a second competing project.

That is the design basis for a future **piCoreDSP v2**.
