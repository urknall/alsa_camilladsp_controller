#!/usr/bin/env sh
# piCoreCDSP v2 — Capability probe convenience wrapper
# roadmap §42, Gate 10
#
# Runs the Python capability probes against a CamillaDSP binary.
# This script is a thin wrapper; the probes live in probe_camilla_capabilities.py.
#
# Usage:
#   probes/probe_camilla_capabilities.sh [--binary /path/to/camilladsp]
#
# If --binary is not given, the script looks for camilladsp in the PATH
# or in ./target/release/camilladsp (for local development).

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CAPABILITIES="${REPO_ROOT}/upstream/capabilities.yml"
OUTPUT="${TMPDIR:-/tmp}/probe-results-$(date +%Y%m%d-%H%M%S).json"

# ── Binary selection ─────────────────────────────────────────────────────────
BINARY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --binary) BINARY="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

if [ -z "$BINARY" ]; then
    if [ -f "${REPO_ROOT}/target/release/camilladsp" ]; then
        BINARY="${REPO_ROOT}/target/release/camilladsp"
    elif command -v camilladsp >/dev/null 2>&1; then
        BINARY="$(command -v camilladsp)"
    else
        echo "ERROR: camilladsp binary not found." >&2
        echo "  Pass --binary /path/to/camilladsp, or put it in PATH." >&2
        exit 1
    fi
fi

BRANCH="local"
if command -v git >/dev/null 2>&1; then
    BRANCH="$(git -C "$(dirname "$BINARY")" rev-parse --abbrev-ref HEAD 2>/dev/null || echo local)"
fi

echo "Probing CamillaDSP binary: ${BINARY}"
echo "Branch label:              ${BRANCH}"
echo "Output:                    ${OUTPUT}"
echo ""

python3 "${SCRIPT_DIR}/probe_camilla_capabilities.py" \
    --binary  "${BINARY}" \
    --branch  "${BRANCH}" \
    --capabilities "${CAPABILITIES}" \
    --output  "${OUTPUT}"

echo ""
echo "Full results: ${OUTPUT}"
