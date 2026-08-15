# Gate 14 — Experimental Real-Hardware Field Test Log

Milestone M14: Experimental real-hardware release

Fill in this log while performing the field test.  Each scenario maps directly
to the checklist items in `ROADMAP_CHECKLIST.md` under **Milestone M14**.

When every scenario under **Pass criteria** is recorded as ✅, Gate 14 is
passed and M14 items can be marked `[x]` in the checklist.

---

## Hardware under test

| Field | Value |
|---|---|
| Raspberry Pi model | |
| piCorePlayer version | |
| picoredsp-controller version / git SHA | |
| libasound_module_pcm_picoredsp.so version / git SHA | |
| DAC make / model | |
| DAC connection (USB / HAT / I2S) | |
| Active backend | `backend = ioplug` |
| Baseline DSP config file | |
| Test date | |
| Tester | |

---

## Prerequisites checklist

- [ ] piCorePlayer installed and boots cleanly
- [ ] `picoredsp-controller` running as a service
- [ ] `libasound_module_pcm_picoredsp.so` installed in `/usr/local/lib/alsa-lib/`
- [ ] `backend = ioplug` confirmed in controller config
- [ ] Valid baseline DSP config loaded and accepted by CamillaGUI
- [ ] DAC visible in `aplay -l`
- [ ] Squeezelite configured to use the picoredsp ALSA PCM

---

## Scenario 1 — Basic playback (44.1 kHz, Squeezelite)

**Steps**
1. Start Squeezelite; queue a 44.1 kHz FLAC or WAV track.
2. Let it play for at least 60 seconds.
3. Check controller log for `START → READY → streaming` sequence.
4. Check `aplay -l` / ALSA xrun counter — should be 0.
5. Listen: confirm audio is audible and distortion-free.

| Check | Result |
|---|---|
| Audio audible | |
| XRUN count after 60 s | |
| Log shows `START` message | |
| Log shows `READY` sent | |
| Log shows streaming active | |
| No crash / no silent discard | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Scenario 2 — Sample-rate changes

**Steps**
1. Play a 44.1 kHz track for 30 s; stop.
2. Immediately play a 48 kHz track; note timestamp of stop and first audio.
3. Repeat: 48 → 96 kHz.
4. Repeat: 96 → 192 kHz (if DAC supports 192 kHz).
5. For each transition, confirm CamillaDSP restarted with the new samplerate in the log.

| Transition | Transition time (s) | CamillaDSP restarted with correct rate | Audio clean |
|---|---|---|---|
| 44.1 → 48 kHz | | | |
| 48 → 96 kHz | | | |
| 96 → 192 kHz | | | |
| 192 → 44.1 kHz | | | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Scenario 3 — AirPlay

**Steps**
1. Connect an iPhone or Mac to the same network.
2. Select the piCorePlayer AirPlay endpoint.
3. Play a track for at least 30 s.
4. Confirm format negotiation appears in the controller log.

| Check | Result |
|---|---|
| AirPlay source detected | |
| Audio audible | |
| Format logged (rate / format / channels) | |
| No XRUN | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial  
**N/A if no AirPlay source available:** ☐

**Notes:**

---

## Scenario 4 — Bluetooth

**Steps**
1. Connect a Bluetooth audio source (phone or BT transmitter).
2. Play a track for at least 30 s.
3. Confirm audio plays end-to-end.

| Check | Result |
|---|---|
| BT source detected by piCorePlayer | |
| Audio audible | |
| No XRUN | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial  
**N/A if no Bluetooth source available:** ☐

**Notes:**

---

## Scenario 5 — Reboot persistence

**Steps**
1. Open CamillaGUI; apply a change to the DSP pipeline (e.g., add a gain step).
2. Click **Save** (not just Apply).
3. Reboot the Pi.
4. After reboot: confirm controller starts, loads the saved config, and play a track.

| Check | Result |
|---|---|
| Config saved in GUI | |
| Pi rebooted cleanly | |
| Controller started automatically | |
| Saved config loaded (log confirms) | |
| Audio plays correctly after reboot | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Scenario 6 — Controller restart mid-stream

**Steps**
1. Start playback.
2. While audio is playing, run: `sudo killall picoredsp-controller`
3. Observe: active ALSA stream should fail cleanly (expected: no reconnect in v1).
4. Wait 5 s; restart the controller: `sudo systemctl restart picoredsp-controller`
5. Play a new track; confirm playback resumes.

| Check | Result |
|---|---|
| Stream failed cleanly (no hang, no silent audio) | |
| ALSA application reported an error | |
| Controller restarted without errors | |
| Playback resumed on next play | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Scenario 7 — CamillaDSP failure mid-stream

**Steps**
1. Start playback.
2. While audio is playing, find the CamillaDSP PID (`pgrep camilladsp`) and kill it:
   `sudo kill <pid>`
3. Observe: plugin should receive EPIPE; ALSA stream terminates cleanly.
4. Check controller log for failure record.
5. Play a new track; confirm playback resumes.

| Check | Result |
|---|---|
| ALSA stream terminated cleanly (no hang) | |
| Controller log shows failure entry | |
| No silent discard | |
| Playback resumed on next play | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Scenario 8 — Invalid DSP config

**Steps**
1. Open CamillaGUI; paste a deliberately broken config (e.g., bad YAML or nonexistent filter file).
2. Click **Apply** (do not save).
3. Attempt to start playback.
4. Confirm ALSA start fails with a meaningful error; confirm no audio is silently discarded.

| Check | Result |
|---|---|
| Controller returned `ERROR_CONFIG` (log) | |
| ALSA start failed (application error) | |
| No silent audio discard | |
| Good config restored; playback works again | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Scenario 9 — DAC unavailable

**Steps**
1. Unplug the USB DAC (or disable the HAT via device tree if applicable).
2. Attempt to start playback.
3. Confirm `ERROR_PLAYBACK_DEVICE` is returned; no hang.
4. Reconnect the DAC; confirm playback resumes.

| Check | Result |
|---|---|
| Controller returned `ERROR_PLAYBACK_DEVICE` (log) | |
| ALSA start failed cleanly | |
| No hang / no silent discard | |
| Playback resumed after DAC reconnected | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial  
**N/A if DAC cannot be unplugged at runtime:** ☐

**Notes:**

---

## Scenario 10 — GUI Apply and Save

**Steps**
1. Start playback.
2. Open CamillaGUI; make a change (e.g., adjust a filter gain) and click **Apply**.
3. Confirm CamillaDSP restarted with the new config (log + audible effect).
4. Click **Save**; reboot; confirm the change persists.

| Check | Result |
|---|---|
| Apply triggered CamillaDSP restart | |
| New config audible / measurable | |
| Config saved successfully | |
| Config persisted after reboot | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Scenario 11 — Rapid format changes

**Steps**
1. In the Squeezelite queue, interleave tracks at 44.1, 48, 96, and 192 kHz.
2. Start playback and rapidly skip through tracks (at least 10 transitions within 2 minutes).
3. Observe: no crashes, no silent discards, no XRUN storm.
4. Check controller log for clean transition messages throughout.

| Check | Result |
|---|---|
| No crash observed | |
| No silent audio discard | |
| XRUN count after test | |
| Controller recovered cleanly for each transition | |
| Total transitions performed | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Scenario 12 — Long-duration stability (24 h)

**Steps**
1. Configure Squeezelite to loop a playlist of mixed sample rates.
2. Start playback; record start time.
3. Check every 4–6 hours: audio still playing, no crash, controller RSS stable.
4. After 24 h: record final XRUN count and controller RSS.

| Measurement | Value |
|---|---|
| Start time | |
| End time | |
| Total runtime (h) | |
| XRUN count at end | |
| Controller RSS at start (kB) | |
| Controller RSS at end (kB) | |
| Any crash / restart observed | |
| PCM corruption heard | |

**Outcome:** ✅ Pass / ❌ Fail / ⚠ Partial

**Notes:**

---

## Overall Gate 14 result

| Scenario | Outcome |
|---|---|
| 1 — Basic playback | |
| 2 — Sample-rate changes | |
| 3 — AirPlay | |
| 4 — Bluetooth | |
| 5 — Reboot persistence | |
| 6 — Controller restart mid-stream | |
| 7 — CamillaDSP failure mid-stream | |
| 8 — Invalid DSP config | |
| 9 — DAC unavailable | |
| 10 — GUI Apply and Save | |
| 11 — Rapid format changes | |
| 12 — 24 h stability | |

**Gate 14 decision:** ✅ PASSED / ❌ FAILED / ⚠ CONDITIONAL PASS (list conditions)

**Conditions / outstanding issues:**

**Signed off by:**

**Date:**
