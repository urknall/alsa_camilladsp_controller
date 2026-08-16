#!/usr/bin/env python3
"""
piCoreCDSP v2 — Black-Box CamillaDSP Capability Probes
roadmap §42, Gate 10

Each probe answers one yes/no question about CamillaDSP's capabilities.
When a probe turns green (capability now exists upstream) the corresponding
local workaround in piCoreCDSP becomes a removal candidate — see
upstream/capabilities.yml and the Removal Matrix in ROADMAP_CHECKLIST_v2.md.

Usage:
    python3 probe_camilla_capabilities.py \\
        --binary /path/to/camilladsp \\
        --branch master \\
        --capabilities ../upstream/capabilities.yml \\
        --output /tmp/probe-results.json

Requires: Python ≥3.9, websockets ≥11 (pip install websockets pyyaml)

The script starts CamillaDSP with a File-backend config (no real hardware),
waits for the WebSocket server, runs each probe, then shuts down.  Probes
that require snd-aloop are automatically skipped if the kernel module is
not available.
"""

import argparse
import asyncio
import json
import os
import platform
import random
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import yaml

try:
    import websockets
    from websockets.client import connect as ws_connect
except ImportError:
    print("ERROR: websockets library not found.  Install with: pip install websockets")
    sys.exit(1)

# ── Minimal CamillaDSP config that uses the File backend (no hardware) ──────
# Adjust field names here if v5 changes the schema.

_CONFIG_V4 = """\
devices:
  samplerate: 44100
  chunksize: 1024
  queuelimit: 4
  silence_threshold: 0
  silence_timeout: 0.0
  stop_on_inactive: true
  capture:
    type: File
    filename: /dev/zero
    format: S32LE
    channels: 2
    skip_bytes: 0
    read_bytes: 0
  playback:
    type: File
    filename: /dev/null
    format: S32LE
    channels: 2
filters: {}
mixers: {}
pipeline: []
"""

# Config with a $samplerate$ token to test token re-resolution.
_CONFIG_WITH_SAMPLERATE_TOKEN = """\
devices:
  samplerate: $samplerate$
  chunksize: 1024
  queuelimit: 4
  silence_threshold: 0
  silence_timeout: 0.0
  stop_on_inactive: true
  capture:
    type: File
    filename: /dev/zero
    format: S32LE
    channels: 2
    skip_bytes: 0
    read_bytes: 0
  playback:
    type: File
    filename: /dev/null
    format: S32LE
    channels: 2
filters: {}
mixers: {}
pipeline: []
"""


# ── Probe results ────────────────────────────────────────────────────────────

PROBE_PASS = "PASS"
PROBE_FAIL = "FAIL"
PROBE_SKIP = "SKIP"
PROBE_ERROR = "ERROR"


def make_result(
    capability: str,
    status: str,
    note: str,
    details: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "capability": capability,
        "status": status,
        "note": note,
        "details": details or {},
    }


# ── WebSocket helpers ────────────────────────────────────────────────────────

async def ws_send_recv(ws, command: str, value: Any = None) -> Any:
    """Send a single command to CamillaDSP and return the parsed response."""
    if value is None:
        msg = json.dumps({command: None})
    else:
        msg = json.dumps({command: value})
    await ws.send(msg)
    raw = await asyncio.wait_for(ws.recv(), timeout=5.0)
    return json.loads(raw)


async def wait_for_camilla(port: int, timeout: float = 15.0) -> bool:
    """Poll until CamillaDSP WebSocket accepts connections or timeout."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            async with ws_connect(f"ws://127.0.0.1:{port}") as ws:
                resp = await ws_send_recv(ws, "GetState")
                if "GetState" in resp:
                    return True
        except Exception:
            pass
        await asyncio.sleep(0.3)
    return False


# ── Individual probes ────────────────────────────────────────────────────────

async def probe_subscribe_state(port: int) -> dict[str, Any]:
    """
    Probe: does SubscribeState exist and push at least one state notification?

    Maps to: upstream/capabilities.yml key state_push_events
    Removal: when SubscribeState is confirmed stable, delete PollingTrigger
             fallback in src/rate_sync/mod.rs.
    """
    try:
        async with ws_connect(f"ws://127.0.0.1:{port}") as ws:
            await ws.send(json.dumps({"SubscribeState": None}))
            # The first response should confirm subscription; subsequent messages
            # are push notifications.
            resp = await asyncio.wait_for(ws.recv(), timeout=5.0)
            parsed = json.loads(resp)
            if "Error" in parsed or "error" in str(parsed).lower():
                return make_result(
                    "state_push_events",
                    PROBE_FAIL,
                    "SubscribeState returned an error",
                    {"response": parsed},
                )
            # Wait for at least one push notification.
            try:
                push = json.loads(await asyncio.wait_for(ws.recv(), timeout=3.0))
                return make_result(
                    "state_push_events",
                    PROBE_PASS,
                    "SubscribeState accepted and sent a push notification",
                    {"subscription_response": parsed, "first_push": push},
                )
            except asyncio.TimeoutError:
                return make_result(
                    "state_push_events",
                    PROBE_FAIL,
                    "SubscribeState accepted but no push notification arrived within 3 s",
                    {"subscription_response": parsed},
                )
    except Exception as exc:
        return make_result("state_push_events", PROBE_ERROR, str(exc))


async def probe_get_previous_config(port: int, config_text: str) -> dict[str, Any]:
    """
    Probe: does GetPreviousConfig return a valid config after a SetConfig call?

    Maps to: upstream/capabilities.yml key persistent_source_rate_override
    Removal: when SetConfig / stop cycle preserves config correctly upstream.
    """
    try:
        async with ws_connect(f"ws://127.0.0.1:{port}") as ws:
            # Apply the config.
            set_resp = await ws_send_recv(ws, "SetConfig", config_text)
            # Retrieve PreviousConfig.
            prev_resp = await ws_send_recv(ws, "GetPreviousConfig")
            if "GetPreviousConfig" not in prev_resp:
                return make_result(
                    "persistent_source_rate_override",
                    PROBE_FAIL,
                    "GetPreviousConfig not present in response",
                    {"response": prev_resp},
                )
            prev_val = prev_resp["GetPreviousConfig"]
            if prev_val is None:
                return make_result(
                    "persistent_source_rate_override",
                    PROBE_FAIL,
                    "GetPreviousConfig returned null",
                    {"set_response": set_resp},
                )
            return make_result(
                "persistent_source_rate_override",
                PROBE_PASS,
                "GetPreviousConfig returned a non-null config after SetConfig",
                {"config_length": len(str(prev_val))},
            )
    except Exception as exc:
        return make_result("persistent_source_rate_override", PROBE_ERROR, str(exc))


async def probe_config_file_path_stability(
    port: int, config_text: str, config_path: str
) -> dict[str, Any]:
    """
    Probe: does ConfigFilePath stay unchanged across a SetConfig call?

    Maps to: upstream/capabilities.yml key config_revision_cas
    Removal: when SetConfig is confirmed not to change ConfigFilePath.
    """
    try:
        async with ws_connect(f"ws://127.0.0.1:{port}") as ws:
            # Get ConfigFilePath before SetConfig.
            before = await ws_send_recv(ws, "GetConfigFilePath")
            # Apply a new config.
            await ws_send_recv(ws, "SetConfig", config_text)
            # Get ConfigFilePath after SetConfig.
            after = await ws_send_recv(ws, "GetConfigFilePath")

            before_path = before.get("GetConfigFilePath")
            after_path = after.get("GetConfigFilePath")

            if before_path == after_path:
                return make_result(
                    "config_revision_cas",
                    PROBE_PASS,
                    "ConfigFilePath unchanged after SetConfig",
                    {"path_before": before_path, "path_after": after_path},
                )
            else:
                return make_result(
                    "config_revision_cas",
                    PROBE_FAIL,
                    "ConfigFilePath changed after SetConfig",
                    {"path_before": before_path, "path_after": after_path},
                )
    except Exception as exc:
        return make_result("config_revision_cas", PROBE_ERROR, str(exc))


async def probe_set_config_value(port: int) -> dict[str, Any]:
    """
    Probe: does SetConfigValue exist and accept a rate field update?

    Maps to: upstream/capabilities.yml key persistent_source_rate_override
    """
    try:
        async with ws_connect(f"ws://127.0.0.1:{port}") as ws:
            resp = await ws_send_recv(
                ws,
                "SetConfigValue",
                {"pointer": "/devices/samplerate", "value": 48000},
            )
            if "Error" in resp or "error" in str(resp).lower():
                return make_result(
                    "set_config_value",
                    PROBE_FAIL,
                    "SetConfigValue returned an error",
                    {"response": resp},
                )
            return make_result(
                "set_config_value",
                PROBE_PASS,
                "SetConfigValue accepted samplerate patch",
                {"response": resp},
            )
    except Exception as exc:
        return make_result("set_config_value", PROBE_ERROR, str(exc))


async def probe_samplerate_token(port: int, token_config: str) -> dict[str, Any]:
    """
    Probe: are $samplerate$ tokens re-resolved correctly after SetConfig?

    Maps to: upstream/capabilities.yml key samplerate_token_reresolution
    Removal: when CamillaDSP re-resolves $samplerate$ without external help.
    """
    try:
        async with ws_connect(f"ws://127.0.0.1:{port}") as ws:
            # Apply a config with $samplerate$ in it.
            resp = await ws_send_recv(ws, "SetConfig", token_config)
            if "Error" in resp:
                return make_result(
                    "samplerate_token_reresolution",
                    PROBE_FAIL,
                    "SetConfig with $samplerate$ token was rejected",
                    {"response": resp},
                )
            # Read back the running config — if the token was resolved, the
            # returned samplerate field will be a number, not the token string.
            get_resp = await ws_send_recv(ws, "GetConfig")
            config_str = get_resp.get("GetConfig", "")
            if "$samplerate$" in str(config_str):
                return make_result(
                    "samplerate_token_reresolution",
                    PROBE_FAIL,
                    "GetConfig still contains $samplerate$ token after SetConfig",
                    {"excerpt": str(config_str)[:200]},
                )
            return make_result(
                "samplerate_token_reresolution",
                PROBE_PASS,
                "SetConfig with $samplerate$ accepted; GetConfig does not contain raw token",
                {"excerpt": str(config_str)[:200]},
            )
    except Exception as exc:
        return make_result("samplerate_token_reresolution", PROBE_ERROR, str(exc))


async def probe_aloop_available() -> dict[str, Any]:
    """
    Probe: is snd-aloop available in the kernel?

    Maps to: upstream/capabilities.yml key native_aloop_rate_following
    This probe does not require a running CamillaDSP instance.
    """
    if platform.system() != "Linux":
        return make_result(
            "native_aloop_rate_following",
            PROBE_SKIP,
            "Not running on Linux; snd-aloop not applicable",
        )
    # Check via /proc/asound or modprobe.
    try:
        result = subprocess.run(
            ["modprobe", "--dry-run", "snd-aloop"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0:
            return make_result(
                "native_aloop_rate_following",
                PROBE_PASS,
                "snd-aloop module is available (modprobe --dry-run succeeded)",
            )
        return make_result(
            "native_aloop_rate_following",
            PROBE_FAIL,
            "snd-aloop not available (modprobe --dry-run failed)",
            {"stderr": result.stderr[:200]},
        )
    except Exception as exc:
        return make_result("native_aloop_rate_following", PROBE_ERROR, str(exc))


# ── Probe runner ─────────────────────────────────────────────────────────────

async def run_all_probes(
    binary: str,
    branch: str,
    capabilities_path: str,
) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []

    # Probe that does not require a running CamillaDSP.
    results.append(await probe_aloop_available())

    # Write a temporary config file.
    config_path: str | None = None
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".yml", delete=False, prefix="picorecdsp_probe_"
    ) as f:
        f.write(_CONFIG_V4)
        config_path = f.name

    port = random.randint(10100, 10900)
    proc = None

    try:
        # Start CamillaDSP.
        proc = subprocess.Popen(
            [binary, "-p", str(port), config_path],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        # Wait for the WebSocket server to be ready.
        ready = await wait_for_camilla(port, timeout=20.0)
        if not ready:
            results.append(
                make_result(
                    "_startup",
                    PROBE_ERROR,
                    f"CamillaDSP ({branch}) WebSocket did not become ready within 20 s",
                )
            )
            return results

        # Run WebSocket-based probes.
        results.append(await probe_subscribe_state(port))
        results.append(await probe_get_previous_config(port, _CONFIG_V4))
        results.append(
            await probe_config_file_path_stability(port, _CONFIG_V4, config_path)
        )
        results.append(await probe_set_config_value(port))
        results.append(await probe_samplerate_token(port, _CONFIG_WITH_SAMPLERATE_TOKEN))

    finally:
        if proc is not None:
            proc.send_signal(signal.SIGTERM)
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        if config_path is not None and os.path.exists(config_path):
            os.unlink(config_path)

    return results


# ── Main ─────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="Probe CamillaDSP capabilities")
    parser.add_argument("--binary", required=True, help="Path to camilladsp binary")
    parser.add_argument("--branch", required=True, help="Git branch label (informational)")
    parser.add_argument(
        "--capabilities",
        required=True,
        help="Path to upstream/capabilities.yml",
    )
    parser.add_argument("--output", required=True, help="Output JSON file path")
    args = parser.parse_args()

    if not Path(args.binary).is_file():
        print(f"ERROR: binary not found: {args.binary}")
        sys.exit(1)

    results = asyncio.run(
        run_all_probes(args.binary, args.branch, args.capabilities)
    )

    output = {
        "branch": args.branch,
        "binary": args.binary,
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "probes": results,
    }

    Path(args.output).write_text(json.dumps(output, indent=2))
    print(f"Results written to {args.output}")

    # Print a brief summary to stdout.
    passed = sum(1 for r in results if r["status"] == PROBE_PASS)
    failed = sum(1 for r in results if r["status"] == PROBE_FAIL)
    skipped = sum(1 for r in results if r["status"] in (PROBE_SKIP, PROBE_ERROR))
    print(f"Probes: {passed} PASS  {failed} FAIL  {skipped} SKIP/ERROR")
    for r in results:
        icon = {"PASS": "✅", "FAIL": "❌", "SKIP": "⏭", "ERROR": "⚠️"}.get(
            r["status"], "?"
        )
        print(f"  {icon} {r['capability']}: {r['note']}")


if __name__ == "__main__":
    main()
