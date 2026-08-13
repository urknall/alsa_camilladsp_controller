# Gate 0 Acceptance Specification — snd-aloop Baseline

This document defines the baseline acceptance behavior for the current
`backend=aloop` architecture and the acceptance tests that protect it.

Scope: current Rust controller + `snd-aloop` + CamillaDSP websocket behavior.

## Environment assumptions

- ALSA loopback controls are available on the target (`hw:Loopback,0`).
- CamillaDSP websocket endpoint is reachable in normal operation.
- Active config selection is represented by the `active_config.yml` symlink.

## Acceptance scenarios

| Scenario | Expected behavior | Verification |
|---|---|---|
| Idle reboot | If source is inactive at startup, controller does not preload capture via `SetConfig` and enforces idle invariant when CamillaDSP is already running | Automated: `bootstrap_does_not_open_capture_for_inactive_source`, `bootstrap_stops_running_cdsp_when_source_is_inactive` |
| First playback | First active stream after idle startup triggers `Stop` + adapted `SetConfig` using live stream parameters | Automated: `inactive_startup_then_first_playback_applies_live_wave` |
| 44.1 → 48 → 96 kHz changes | Each start/restart adapts runtime samplerate to live stream rate | Automated: `acceptance_rate_change_sequence_reapplies_live_rates` |
| Format changes | Runtime config adaptation tracks negotiated sample format | Automated: `acceptance_format_change_reapplies_live_format` |
| Stop / start | Source stop triggers CamillaDSP stop path; next start re-applies config with live wave | Automated: `normal_source_stop_arms_idle_stop_guard`, `idle_invariant_clears_on_source_active_and_applies_config` |
| GUI Apply and Save | Unsaved in-memory GUI state is not treated as persistent baseline; persisted file/symlink changes are re-read on restart | Automated: `acceptance_gui_apply_and_save_rereads_saved_active_file`; Manual: GUI Apply vs Save workflow check |
| Active-config selection | Controller follows active symlink target and uses selected config on next start | Automated: `start_cdsp_follows_symlink_retarget` |
| PCP backup | Persisted config changes survive reboot only after `pcp backup` | Manual on piCorePlayer target |
| Reboot persistence | Controller restart/boot reads persisted active config and applies only when source becomes active | Automated: bootstrap tests above + Manual reboot verification |
| Controller restart | Restart while source inactive remains idle; restart while source active applies live-adapted config | Automated: bootstrap tests + first-playback test |
| CamillaDSP restart | If CamillaDSP becomes inactive while source remains active, controller re-applies runtime config | Automated: `acceptance_cdsp_restart_with_active_source_restarts_processing` |
| Transient WebSocket failure | Transient transport errors are surfaced so outer supervisor/restart policy can recover | Automated: `acceptance_transient_websocket_failure_returns_error` |

## Manual acceptance checklist (hardware)

Run on piCorePlayer hardware with `snd-aloop` enabled:

1. Reboot with no playback active; confirm CamillaDSP is not preloaded from controller.
2. Start playback at 44.1 kHz; confirm runtime samplerate = 44.1 kHz.
3. Switch tracks to 48 kHz then 96 kHz; confirm each transition is reflected.
4. Stop playback; confirm controller enforces idle state.
5. In CamillaGUI, use Apply without Save; force restart/format change; confirm unsaved state is not persisted.
6. Use Apply and Save; run `pcp backup`; reboot; confirm saved state persists.
7. Restart controller while source inactive and while source active; verify expected behavior.
8. Restart/kill CamillaDSP while source active; confirm controller restarts processing.
9. Simulate transient websocket disruption; confirm controller exits/errors for supervisor recovery.
