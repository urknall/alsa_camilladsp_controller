# alsa_camilladsp_controller — code review, fixes, and runbook

Review date: 2026-08-14

## Executive summary

The repository implements two audio backends around a common Rust controller:

- **aloop (recommended/stable)** — source applications write to `pcm.picoredsp`, which routes to `hw:Loopback,1,0`; `snd-aloop` exposes the stream at `hw:Loopback,0,0`; CamillaDSP captures it; the Rust controller watches ALSA HCTL controls and adapts/reloads the CamillaDSP configuration.
- **ioplug (experimental)** — source applications write to a custom ALSA PCM plugin. The plugin negotiates stream parameters with the Rust controller over AF_UNIX, the controller writes a transient adapted config and starts CamillaDSP with stdin capture, then passes the pipe write fd to the C plugin using SCM_RIGHTS. PCM flows directly from the C plugin to CamillaDSP; Rust stays out of the PCM data path.

The overall architecture is sensible: backend-neutral Rust core/state/config logic, backend-specific stream detection/transport, a small C ioplug, explicit IPC framing, and a piCorePlayer installer that can stage either route.

The ioplug backend should still be treated as **experimental**. The repository's own roadmap still has real-hardware release, 24 h / 7 day soak, rate-transition timing, fault injection, latency tuning, configuration migration, upstream tracking, and production-promotion work open.

## Fixes applied

### High impact

1. **C worker shutdown could hang indefinitely**
   - Previous code closed `pipe_fd` from a different thread and assumed that would interrupt a worker blocked in `write()`.
   - Linux does not guarantee that behavior; a local reproduction remained blocked after the other thread closed the fd.
   - Fix: production pipe fd is set `O_NONBLOCK`, writes use bounded `poll(POLLOUT)` waits, and an atomic `worker_running` cancellation flag is checked. Worker joinability is tracked separately from worker liveness so a worker that exits on EPIPE is still reaped.
   - Added a regression test that fills a nonblocking pipe, starts a drain thread, cancels it, and requires a bounded join.

2. **ioplug installer unnecessarily required `snd-aloop`**
   - `--backend ioplug` still ran `modprobe snd-aloop` and the HCTL probe.
   - Fix: both checks now run only for the `aloop` backend. Direct ioplug installation no longer fails merely because loopback support is absent.

3. **CMake installed the ALSA module under the wrong filename**
   - The build created an unprefixed module plus a build-tree symlink, but `cmake --install` installed the unprefixed target.
   - ALSA discovers external PCM modules as `libasound_module_pcm_<type>.so`.
   - Fix: the real target filename is now `libasound_module_pcm_picoredsp.so`; the symlink workaround was removed.

4. **Malformed/half-open ioplug clients could terminate the Rust controller loop**
   - A per-client HELLO/START error propagated as a backend/controller error.
   - Fix: client protocol failures are rejected and dropped; after HELLO the peer receives `ERROR(PROTOCOL)` where possible, and the listener remains alive.
   - Added a regression test proving a malformed client can be followed by a valid client.

### Medium impact

5. **Unsafe stale-socket cleanup**
   - Binding blindly removed whatever existed at the configured AF_UNIX path.
   - Fix: Rust now uses `symlink_metadata` and refuses to remove regular files/symlinks. Added a regression test.

6. **IPC validation gaps in the C client**
   - Overlong AF_UNIX paths could be silently truncated.
   - A HELLO reply newer than the offered version was silently clamped locally.
   - ERROR frames did not validate negotiated version or error-code range.
   - Fixes and regression tests added for all three cases.

7. **ALSA callback error reporting discarded useful errors**
   - `hw_params` converted most READY/IPC failures to `-EINVAL`.
   - Fix: CONFIG → `-EINVAL`, PLAYBACK_DEVICE → `-ENODEV`, PROTOCOL → `-EPROTO`, INTERNAL → `-EIO`; transport errno is preserved.
   - `delay` and `drain` now surface a recorded stream error rather than reporting success after a failed consumer.

8. **Worker thread creation reported the wrong errno**
   - Any `pthread_create` failure became `-ENOMEM`.
   - Fix: return the actual pthread error code.

9. **The positive READY integration test did not test a valid Gate-8 handshake**
   - It accepted either success or `-EPROTO` and did not send the required SCM_RIGHTS fd.
   - Fix: mock controller now sends READY plus a real pipe write fd; the test requires success.

10. **Retry log reported fictitious backoff durations**
    - Runtime sequence is 500 ms → 1 s → 2 s → 5 s → 10 s → 30 s, but log output used an unrelated `consecutive * 5 seconds` expression.
    - Fix: retry state exposes the scheduled duration and logging prints that value. Added a sequence test.

11. **C test suite was not a release build dependency in CI**
    - The workflow built/released the plugin but did not run its CTest suite as a gating job.
    - Fix: native GCC and Clang CTest matrix added; cross-release build now depends on it.

12. **TSAN found a data race/use-after-scope risk in the IPC test harness**
    - Mock server threads were detached while holding a pointer to a stack `mock_server_t`; `accepted_fd` was concurrently accessed without synchronization.
    - Fix: server thread is joinable, lifecycle is joined before the stack object goes away, and shared accepted-fd state is atomic.

13. **Release/benchmark compile warning in `timing.c`**
    - GCC `-O2 -Werror` reported `now` as potentially uninitialized across the clock helper call.
    - Fix: defensive initialization. This allows release-mode benchmark sources to compile cleanly with GCC and Clang.

14. **Documentation overstated CI sanitizer/static-analysis coverage**
    - CMake options existed, but the workflow did not run ASAN/UBSAN/TSAN or clang-tidy.
    - Fix: roadmap wording now reflects reality instead of marking those CI requirements complete. GCC/Clang native tests are marked complete; sanitizer/static-analysis CI remains open.

## Verification performed in this environment

The environment did **not** contain Rust (`cargo`/`rustc`) or ALSA development headers, so a truthful full repository build was not possible locally. The checks below were executed on the parts that can be built without those dependencies.

### C suites

The following five suites were compiled and run:

- ringbuffer: 11 tests
- timing: 6 tests
- PCM worker: 20 tests
- audio integrity: 11 tests
- IPC: 34 tests

Total: **82 tests per configuration**.

Passing configurations:

- GCC debug/warning build: **82/82**
- Clang debug/warning build: **82/82**
- GCC + AddressSanitizer: **82/82**
- GCC + UndefinedBehaviorSanitizer: **82/82**
- GCC + ThreadSanitizer: **82/82**
- GCC `-O2 -Werror`: compile clean
- Clang `-O2 -Werror`: compile clean

The three C benchmark binaries also compile with GCC and Clang at `-O2 -Werror`; GCC benchmark executables were run successfully.

### Other checks

- `sh -n install_picoredsp.sh`: pass
- `dash -n install_picoredsp.sh`: pass
- GitHub Actions workflow YAML parse: pass
- Source-hygiene check for accidental call-site rewrite artifacts: pass

### Not locally verified

- Rust formatting, Clippy, MSRV compile and Rust unit tests — no Rust toolchain installed in this sandbox.
- Full CMake plugin build and `test_pcm_integration` — no `libasound2-dev`/ALSA headers in this sandbox.
- ARM cross-builds — no ARM cross toolchain/ALSA multiarch setup here.
- Real CamillaDSP + DAC playback.
- Real piCorePlayer installer execution/reboot.
- 24 h / 7 day soak, rate-switch timing, DAC fault injection, external end-to-end latency.

The patched GitHub workflow is intended to execute the missing full native CMake/ALSA integration checks in a normal Ubuntu CI environment.

## Remaining production risks / follow-up work

### 1. Immediate CamillaDSP exit is still misclassified

In ioplug mode, if CamillaDSP exits during the startup window, the controller treats **both** bad config and playback-device failure as `ErrorCode::Config` and latches until the config file changes. A temporarily unavailable DAC can therefore look permanent, and the newly improved C mapping for `PLAYBACK_DEVICE → -ENODEV` is not used for this path.

Recommended follow-up: separate static config validation (`camilladsp --check <runtime-config>`) from runtime device-open failure, or capture/classify startup failure output. Bad configuration can remain latched; playback-device failure should use `PLAYBACK_DEVICE` and a transient/recoverable policy.

I did not change this behavior without a local Rust/CamillaDSP build because an incorrect classifier could turn genuine bad configs into restart loops or mislabel device failures.

### 2. Sanitizer/static-analysis CI is still not wired in

ASAN/UBSAN/TSAN and clang-tidy switches exist in CMake. The independently buildable core suites pass the three sanitizers locally after the fixes, but the GitHub workflow still does not run those sanitizer/static-analysis configurations. The roadmap now records that accurately.

### 3. Real-hardware qualification remains incomplete

The roadmap still calls for rate-transition measurements, plugin-overhead measurement, 24-hour and 7-day stability, deliberate DAC error recovery, per-rate latency tuning, configuration migration, upstream tracking, and the final production-promotion decision.

## How to build and test on Debian/Ubuntu

Install prerequisites:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libasound2-dev cmake clang
```

Install Rust stable plus the declared MSRV (Rust 1.71) with rustup, then from the repository root:

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo +1.71 check --locked
cargo build --release --locked
```

Build/test the C plugin:

```sh
cmake -S picoredsp-ioplug -B picoredsp-ioplug/build \
  -DCMAKE_BUILD_TYPE=Debug
cmake --build picoredsp-ioplug/build --parallel
ctest --test-dir picoredsp-ioplug/build --output-on-failure
```

Release build:

```sh
cmake -S picoredsp-ioplug -B picoredsp-ioplug/build-release \
  -DCMAKE_BUILD_TYPE=Release
cmake --build picoredsp-ioplug/build-release --parallel \
  --target asound_module_pcm_picoredsp
```

The plugin output should be:

```text
picoredsp-ioplug/build-release/libasound_module_pcm_picoredsp.so
```

Optional sanitizer builds:

```sh
cmake -S picoredsp-ioplug -B picoredsp-ioplug/build-asan -DASAN=ON -DCMAKE_BUILD_TYPE=Debug
cmake --build picoredsp-ioplug/build-asan --parallel
ctest --test-dir picoredsp-ioplug/build-asan --output-on-failure

cmake -S picoredsp-ioplug -B picoredsp-ioplug/build-ubsan -DUBSAN=ON -DCMAKE_BUILD_TYPE=Debug
cmake --build picoredsp-ioplug/build-ubsan --parallel
ctest --test-dir picoredsp-ioplug/build-ubsan --output-on-failure

cmake -S picoredsp-ioplug -B picoredsp-ioplug/build-tsan -DTSAN=ON -DCMAKE_BUILD_TYPE=Debug
cmake --build picoredsp-ioplug/build-tsan --parallel
ctest --test-dir picoredsp-ioplug/build-tsan --output-on-failure
```

## How to run

### piCorePlayer — recommended route

Run the installer **as user `tc`, not via `sudo ./install_picoredsp.sh`**. It uses sudo internally only where needed:

```sh
chmod +x install_picoredsp.sh
./install_picoredsp.sh --backend aloop
```

For the experimental direct path:

```sh
./install_picoredsp.sh --backend ioplug
```

After installation, backend selection can be changed for the next boot with:

```sh
/usr/local/bin/picoredsp-switch-backend aloop
/usr/local/bin/picoredsp-switch-backend ioplug
```

### Manual aloop controller run

CamillaDSP must already be running and capturing the loopback endpoint. Then:

```sh
sudo modprobe snd-aloop
./target/release/picoredsp-controller \
  --backend aloop \
  --device hw:Loopback,0 \
  --adapt /path/to/active_config.yml \
  --host 127.0.0.1 \
  --port 1234 \
  --log-level INFO
```

ALSA route:

```text
application -> pcm.picoredsp -> hw:Loopback,1,0
            -> snd-aloop -> hw:Loopback,0,0 -> CamillaDSP -> DAC
```

### Manual ioplug controller run

Install `libasound_module_pcm_picoredsp.so` into the ALSA plugin directory and define a PCM similar to:

```text
pcm.picoredsp {
    type plug
    slave {
        pcm {
            type picoredsp
            socket_path "/run/picoredsp/control.sock"
        }
        channels 2
    }
}
```

Then run the controller:

```sh
mkdir -p /run/picoredsp
./target/release/picoredsp-controller \
  --backend ioplug \
  --socket-path /run/picoredsp/control.sock \
  --camilladsp /path/to/camilladsp \
  --adapt /path/to/active_config.yml \
  --host 127.0.0.1 \
  --port 1234 \
  --cdsp-statefile /path/to/statefile.yml \
  --log-level INFO
```

ALSA route:

```text
application -> pcm.picoredsp -> C ioplug
            -> AF_UNIX control + SCM_RIGHTS pipe fd
            -> CamillaDSP stdin -> DAC
```

## Suggested next validation sequence on the target Pi

1. Run all Rust and C CI checks on a normal Ubuntu host.
2. Build ARM release artifacts exactly as GitHub Actions does.
3. Install `aloop`, verify existing playback/regression behavior first.
4. Install `ioplug`, verify S16/S24/S32/F32 playback at intended sample rates.
5. Exercise pause/resume/drain/drop and rapid open/close.
6. Kill/restart the controller during a stream; kill CamillaDSP during a stream.
7. Deliberately make the DAC unavailable and verify user-visible errno/recovery behavior.
8. Run 44.1↔48 and 48↔96 kHz transitions repeatedly.
9. Run 24 h, then 7 day soak with XRUN/process/RSS logging.
10. Only then tune period/buffer/chunksize/queuelimit and compare end-to-end latency against aloop.

