#!/usr/bin/env sh
# piCoreCDSP v2 — Fresh installer for piCorePlayer (roadmap §43, Gate 11)
#
# ┌─────────────────────────────────────────────────────────────────────────┐
# │  FRESH INSTALL ONLY.  No reinstall, no migration, no backup restore.    │
# │  Run this exactly once on a clean pCP image.                            │
# └─────────────────────────────────────────────────────────────────────────┘
#
# What this installer does (roadmap §43):
#   1.  Abort if not running on piCorePlayer.
#   2.  Abort if snd-aloop is not available in the kernel.
#   3.  Detect the physical playback device (once; stored for config generation).
#   4.  Install the pcm.picorecdsp ALSA plug definition.
#   5.  Install the pinned CamillaDSP binary.
#   6.  Install the pinned CamillaGUI backend.
#   7.  Configure the shared native statefile.
#   8.  Generate Bypass.yml and Null.yml (only if absent; never overwrites).
#   9.  Install the piCoreCDSP v2 Rust daemon binary.
#  10.  Register all components for pCP startup (bootlocal.sh).
#  11.  Add installed files to pCP backup list.
#  12.  Run pCP backup (filetool.sh -b).
#  13.  Prompt for reboot.
#
# What this installer does NOT do:
#   ✗  No reinstall / migration path.
#   ✗  No backend menu or backend switcher.
#   ✗  No overwriting of existing user configs.
#   ✗  No Squeezelite parameter management.
#   ✗  No runtime YAML creation.
#   ✗  No shadow config file.
#
# Usage:
#   chmod +x install_picorecdsp.sh
#   sudo ./install_picorecdsp.sh [--playback-device hw:X,Y] [--dry-run]
#
# Pinned stack (set at Gate 12 hardware validation — update these variables):
#   CAMILLA_VERSION   CamillaDSP release tag (e.g. "v2.0.3")
#   CAMILLA_GUI_VERSION  CamillaGUI release tag
#   PICORECDSP_VERSION   piCoreCDSP release tag

set -eu

# ── Version pins (Gate 12 fills these in after hardware validation) ───────────
# Until Gate 12 confirms the final pin, these are illustrative placeholders.
# See roadmap §1: "design for CamillaDSP 5 semantics, ship only against a pinned
# and hardware-validated release stack."
CAMILLA_VERSION="${CAMILLA_VERSION:-GATE12_PIN_REQUIRED}"
CAMILLA_GUI_VERSION="${CAMILLA_GUI_VERSION:-GATE12_PIN_REQUIRED}"
PICORECDSP_VERSION="${PICORECDSP_VERSION:-GATE12_PIN_REQUIRED}"

# ── Installation paths ────────────────────────────────────────────────────────
INSTALL_BIN="/usr/local/bin"
CAMILLA_CONFIG_DIR="/home/tc/CamillaConfigs"
CAMILLA_STATEFILE="/home/tc/camilladsp_statefile.yaml"
CAMILLA_GUI_DIR="/home/tc/camillagui"
ASOUND_CONF="/etc/asound.conf"
BOOTLOCAL="/opt/bootlocal.sh"
FILETOOL_LST="/home/tc/.filetool.lst"

# ── Script dir ────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── Flags ─────────────────────────────────────────────────────────────────────
DRY_RUN=0
PLAYBACK_DEVICE=""

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)           DRY_RUN=1; shift ;;
        --playback-device)   PLAYBACK_DEVICE="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

# ── Helpers ───────────────────────────────────────────────────────────────────

info()  { echo "  [INFO]  $*"; }
ok()    { echo "  [ OK ]  $*"; }
warn()  { echo "  [WARN]  $*" >&2; }
abort() { echo "  [FAIL]  $*" >&2; exit 1; }

run() {
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "  [DRY ]  $*"
    else
        "$@"
    fi
}

require_root() {
    if [ "$(id -u)" -ne 0 ]; then
        abort "This installer must be run as root (sudo ./install_picorecdsp.sh)."
    fi
}

file_is_absent_or_empty() {
    # Returns 0 (true) if path does not exist or is empty.
    [ ! -f "$1" ] || [ ! -s "$1" ]
}

add_to_backup_list() {
    _entry="$1"
    if ! grep -qxF "$_entry" "$FILETOOL_LST" 2>/dev/null; then
        run sh -c "echo '$_entry' >> '$FILETOOL_LST'"
        info "Added $_entry to pCP backup list."
    fi
}

# ── Step 1: Platform check ────────────────────────────────────────────────────
check_platform() {
    echo ""
    echo "=== Step 1: Platform check ==="
    if [ ! -f /etc/os-release ] && [ ! -f /usr/share/doc/pcp/version ]; then
        # piCorePlayer 8.x+ writes version info here
        if ! grep -qi 'picore\|tinycore\|picoreplayer' /etc/issue 2>/dev/null &&
           ! grep -qi 'picore\|tinycore\|picoreplayer' /proc/version 2>/dev/null; then
            abort "Not running on piCorePlayer / TinyCore Linux. Aborting."
        fi
    fi
    ok "piCorePlayer detected."
}

# ── Step 2: snd-aloop check ───────────────────────────────────────────────────
check_aloop() {
    echo ""
    echo "=== Step 2: snd-aloop availability check ==="
    # Check if snd-aloop is already loaded.
    if grep -q "^snd.aloop" /proc/modules 2>/dev/null || \
       grep -q "^snd_aloop" /proc/modules 2>/dev/null; then
        ok "snd-aloop module is loaded."
        return
    fi
    # Try to load it.
    if modprobe snd-aloop 2>/dev/null; then
        ok "snd-aloop loaded successfully."
        return
    fi
    abort "snd-aloop is not available in this kernel build.  " \
          "piCoreCDSP v2 requires snd-aloop.  " \
          "Please use a kernel with snd-aloop support (pCP kernels include it)."
}

# ── Step 3: Playback device detection ────────────────────────────────────────
detect_playback_device() {
    echo ""
    echo "=== Step 3: Physical playback device detection ==="

    if [ -n "$PLAYBACK_DEVICE" ]; then
        ok "Using provided playback device: $PLAYBACK_DEVICE"
        return
    fi

    # List all playback devices, excluding snd-aloop (Loopback).
    if ! command -v aplay >/dev/null 2>&1; then
        warn "aplay not found; skipping auto-detection.  Use --playback-device."
        PLAYBACK_DEVICE="hw:0,0"
        return
    fi

    # Pick the first non-Loopback card.
    DETECTED=$(aplay -l 2>/dev/null \
        | grep "^card " \
        | grep -v -i "loopback" \
        | head -1 \
        | sed 's/card \([0-9]*\): .*, device \([0-9]*\):.*/hw:\1,\2/' \
        || true)

    if [ -z "$DETECTED" ]; then
        warn "No non-loopback playback device detected."
        warn "Set PLAYBACK_DEVICE manually or pass --playback-device hw:X,Y."
        PLAYBACK_DEVICE="hw:0,0"
    else
        PLAYBACK_DEVICE="$DETECTED"
        ok "Detected playback device: $PLAYBACK_DEVICE"
    fi
}

# ── Step 4: Install pcm.picorecdsp ALSA plug ─────────────────────────────────
install_alsa_plug() {
    echo ""
    echo "=== Step 4: Install pcm.picorecdsp ALSA plug ==="

    PCM_CONF="${SCRIPT_DIR}/configs/pcm.picorecdsp.conf"
    if [ ! -f "$PCM_CONF" ]; then
        abort "configs/pcm.picorecdsp.conf not found next to installer. " \
              "Please run from the piCoreCDSP release directory."
    fi

    if [ -f "$ASOUND_CONF" ] && grep -q "pcm.picorecdsp" "$ASOUND_CONF"; then
        ok "$ASOUND_CONF already contains pcm.picorecdsp — skipping."
    else
        info "Appending pcm.picorecdsp block to $ASOUND_CONF ..."
        run sh -c "cat '$PCM_CONF' >> '$ASOUND_CONF'"
        add_to_backup_list "$ASOUND_CONF"
        ok "pcm.picorecdsp installed in $ASOUND_CONF."
    fi
}

# ── Step 5: Install CamillaDSP ────────────────────────────────────────────────
install_camilladsp() {
    echo ""
    echo "=== Step 5: Install CamillaDSP ${CAMILLA_VERSION} ==="

    TARGET_BIN="${INSTALL_BIN}/camilladsp"

    if [ "$CAMILLA_VERSION" = "GATE12_PIN_REQUIRED" ]; then
        abort "CAMILLA_VERSION is not set.  Hardware validation (Gate 12) must " \
              "determine the production pin before this installer can run.  " \
              "Set: export CAMILLA_VERSION=v<x.y.z>"
    fi

    if [ -f "$TARGET_BIN" ]; then
        INSTALLED_VER=$("$TARGET_BIN" --version 2>/dev/null | head -1 || echo "unknown")
        warn "CamillaDSP already installed: $INSTALLED_VER"
        warn "This is a fresh-install-only installer. Aborting to protect existing installation."
        abort "Remove $TARGET_BIN manually if you want a clean reinstall."
    fi

    ARCH="$(uname -m)"
    case "$ARCH" in
        aarch64|arm64) RELEASE_ARCH="aarch64-unknown-linux-musl" ;;
        armv7l)        RELEASE_ARCH="armv7-unknown-linux-musleabihf" ;;
        x86_64)        RELEASE_ARCH="x86_64-unknown-linux-musl" ;;
        *) abort "Unsupported architecture: $ARCH" ;;
    esac

    DOWNLOAD_URL="https://github.com/HEnquist/camilladsp/releases/download/${CAMILLA_VERSION}/camilladsp-${CAMILLA_VERSION}-${RELEASE_ARCH}.tar.gz"
    _DL_TMP="$(mktemp -d)"
    trap 'rm -rf "$_DL_TMP"' EXIT

    info "Downloading CamillaDSP ${CAMILLA_VERSION} for ${RELEASE_ARCH} ..."
    run wget -q -O "${_DL_TMP}/camilladsp.tar.gz" "$DOWNLOAD_URL" \
        || abort "Download failed: $DOWNLOAD_URL"
    run tar -xzf "${_DL_TMP}/camilladsp.tar.gz" -C "${_DL_TMP}"
    run install -m 0755 "${_DL_TMP}/camilladsp" "$TARGET_BIN"

    rm -rf "$_DL_TMP"
    trap - EXIT

    add_to_backup_list "$TARGET_BIN"
    ok "CamillaDSP ${CAMILLA_VERSION} installed at $TARGET_BIN."
}

# ── Step 6: Install CamillaGUI ────────────────────────────────────────────────
install_camillagui() {
    echo ""
    echo "=== Step 6: Install CamillaGUI ${CAMILLA_GUI_VERSION} ==="

    if [ "$CAMILLA_GUI_VERSION" = "GATE12_PIN_REQUIRED" ]; then
        abort "CAMILLA_GUI_VERSION is not set.  Set: export CAMILLA_GUI_VERSION=v<x.y.z>"
    fi

    if [ -d "$CAMILLA_GUI_DIR" ]; then
        warn "CamillaGUI directory already exists at $CAMILLA_GUI_DIR."
        abort "Fresh-install-only.  Remove $CAMILLA_GUI_DIR manually to reinstall."
    fi

    run mkdir -p "$CAMILLA_GUI_DIR"

    # CamillaGUI backend is a Python application.  Install Python + pip if absent.
    if ! command -v python3 >/dev/null 2>&1; then
        info "Installing Python3 via tce-load ..."
        run tce-load -wi python3.9 || warn "tce-load failed; python3 may already be installed another way."
    fi
    if ! command -v pip3 >/dev/null 2>&1; then
        info "Installing pip3 via tce-load ..."
        run tce-load -wi python3.9-dev || true
    fi

    GUI_URL="https://github.com/HEnquist/camillagui-backend/archive/refs/tags/${CAMILLA_GUI_VERSION}.tar.gz"
    _GUI_TMP="$(mktemp -d)"
    trap 'rm -rf "$_GUI_TMP"' EXIT

    info "Downloading CamillaGUI ${CAMILLA_GUI_VERSION} ..."
    run wget -q -O "${_GUI_TMP}/camillagui.tar.gz" "$GUI_URL" \
        || abort "Download failed: $GUI_URL"
    run tar -xzf "${_GUI_TMP}/camillagui.tar.gz" -C "${_GUI_TMP}"
    run cp -r "${_GUI_TMP}/camillagui-backend-"*/* "$CAMILLA_GUI_DIR/"

    rm -rf "$_GUI_TMP"
    trap - EXIT

    info "Installing CamillaGUI Python dependencies ..."
    run pip3 install --quiet -r "${CAMILLA_GUI_DIR}/requirements.txt" \
        || warn "pip install warnings above may be ignorable if deps are already present."

    add_to_backup_list "$CAMILLA_GUI_DIR"
    ok "CamillaGUI ${CAMILLA_GUI_VERSION} installed at $CAMILLA_GUI_DIR."
}

# ── Step 7: Configure shared native statefile ─────────────────────────────────
configure_statefile() {
    echo ""
    echo "=== Step 7: Configure shared native statefile ==="

    if [ -f "$CAMILLA_STATEFILE" ]; then
        ok "Statefile already exists at $CAMILLA_STATEFILE — not overwriting."
        return
    fi

    # Create an empty statefile; CamillaDSP populates it on first run.
    run touch "$CAMILLA_STATEFILE"
    add_to_backup_list "$CAMILLA_STATEFILE"
    ok "Statefile placeholder created at $CAMILLA_STATEFILE."
    info "(CamillaDSP will populate this on first run.)"

    # Write a minimal CamillaGUI config that points at the statefile.
    CAMILLA_GUI_CONF="${CAMILLA_GUI_DIR}/config.yml"
    if [ -f "$CAMILLA_GUI_CONF" ]; then
        info "CamillaGUI config already present — skipping statefile GUI config write."
        return
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "  [DRY ]  Would write ${CAMILLA_GUI_CONF}"
    else
        cat > "$CAMILLA_GUI_CONF" <<EOF
# CamillaGUI v2 configuration — generated by piCoreCDSP installer.
# Do NOT add a statefile entry under camilladsp_path — use the one below.
# Do NOT edit cdsp_* paths; they point to the pinned binaries.
camilla_host: "127.0.0.1"
camilla_port: 1234
port: 5005
config_dir: "${CAMILLA_CONFIG_DIR}"
coeff_dir: "${CAMILLA_CONFIG_DIR}/coeffs"
shared_statefile: "${CAMILLA_STATEFILE}"
EOF
    fi
    ok "CamillaGUI config written."
}

# ── Step 8: Generate Bypass.yml and Null.yml ──────────────────────────────────
generate_configs() {
    echo ""
    echo "=== Step 8: Generate Bypass.yml / Null.yml ==="

    run mkdir -p "$CAMILLA_CONFIG_DIR"
    add_to_backup_list "$CAMILLA_CONFIG_DIR"

    BYPASS_FILE="${CAMILLA_CONFIG_DIR}/Bypass.yml"
    NULL_FILE="${CAMILLA_CONFIG_DIR}/Null.yml"

    # Never overwrite existing user configs (roadmap §43 / §44).
    if [ -f "$BYPASS_FILE" ]; then
        ok "Bypass.yml already exists — not overwriting (user-owned)."
    else
        info "Generating ${BYPASS_FILE} ..."
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would write ${BYPASS_FILE}"
        else
            cat > "${BYPASS_FILE}" <<EOF
# Bypass.yml — generated by piCoreCDSP v2 installer.
# User-owned after installation: the installer never overwrites this file.
# Roadmap §44: matches the pinned CamillaDSP version, no piCoreCDSP tokens.
#
# CamillaDSP version: ${CAMILLA_VERSION}
# Generated by piCoreCDSP installer for playback device: ${PLAYBACK_DEVICE}
#
# Loopback wiring: producers → pcm.picorecdsp → hw:Loopback,1,0 (playback)
#                  snd-aloop routes to  → hw:Loopback,0,0 (capture for CamillaDSP)
# Verify device numbers on real hardware at Gate 12.

devices:
  samplerate: 44100
  chunksize: 1024
  queuelimit: 4
  silence_threshold: 0
  silence_timeout: 0.0
  stop_on_inactive: true
  capture:
    type: ALSACapture
    channels: 2
    device: "hw:Loopback,0,0"
    format: S32LE
  playback:
    type: ALSAPlayback
    channels: 2
    device: "${PLAYBACK_DEVICE}"
    format: S32LE

filters: {}
mixers: {}
pipeline: []
EOF
        fi
        ok "Bypass.yml generated."
    fi

    if [ -f "$NULL_FILE" ]; then
        ok "Null.yml already exists — not overwriting (user-owned)."
    else
        info "Generating ${NULL_FILE} ..."
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would write ${NULL_FILE}"
        else
            cat > "${NULL_FILE}" <<EOF
# Null.yml — generated by piCoreCDSP v2 installer.
# User-owned after installation: the installer never overwrites this file.
# Roadmap §44: matches the pinned CamillaDSP version, no piCoreCDSP tokens.
# Routes audio to /dev/null (silence), useful for testing without a DAC.
#
# CamillaDSP version: ${CAMILLA_VERSION}
#
# Loopback wiring: producers → pcm.picorecdsp → hw:Loopback,1,0 (playback)
#                  snd-aloop routes to  → hw:Loopback,0,0 (capture for CamillaDSP)
# Verify device numbers on real hardware at Gate 12.

devices:
  samplerate: 44100
  chunksize: 1024
  queuelimit: 4
  silence_threshold: 0
  silence_timeout: 0.0
  stop_on_inactive: true
  capture:
    type: ALSACapture
    channels: 2
    device: "hw:Loopback,0,0"
    format: S32LE
  playback:
    type: File
    channels: 2
    filename: /dev/null
    format: S32LE

filters: {}
mixers: {}
pipeline: []
EOF
        fi
        ok "Null.yml generated."
    fi
}

# ── Step 9: Install piCoreCDSP Rust binary ────────────────────────────────────
install_picorecdsp_binary() {
    echo ""
    echo "=== Step 9: Install piCoreCDSP daemon ==="

    TARGET_BIN="${INSTALL_BIN}/picorecdsp"

    if [ "$PICORECDSP_VERSION" = "GATE12_PIN_REQUIRED" ]; then
        abort "PICORECDSP_VERSION is not set.  Set: export PICORECDSP_VERSION=v<x.y.z>"
    fi

    if [ -f "$TARGET_BIN" ]; then
        warn "piCoreCDSP already installed at $TARGET_BIN."
        abort "Fresh-install-only.  Remove $TARGET_BIN manually to reinstall."
    fi

    ARCH="$(uname -m)"
    case "$ARCH" in
        aarch64|arm64) RELEASE_ARCH="aarch64-unknown-linux-musl" ;;
        armv7l)        RELEASE_ARCH="armv7-unknown-linux-musleabihf" ;;
        x86_64)        RELEASE_ARCH="x86_64-unknown-linux-musl" ;;
        *) abort "Unsupported architecture: $ARCH" ;;
    esac

    DOWNLOAD_URL="https://github.com/urknall/piCoreCDSP/releases/download/${PICORECDSP_VERSION}/picorecdsp-${PICORECDSP_VERSION}-${RELEASE_ARCH}.tar.gz"
    _DL_TMP="$(mktemp -d)"
    trap 'rm -rf "$_DL_TMP"' EXIT

    info "Downloading piCoreCDSP ${PICORECDSP_VERSION} for ${RELEASE_ARCH} ..."
    run wget -q -O "${_DL_TMP}/picorecdsp.tar.gz" "$DOWNLOAD_URL" \
        || abort "Download failed: $DOWNLOAD_URL"
    run tar -xzf "${_DL_TMP}/picorecdsp.tar.gz" -C "${_DL_TMP}"
    run install -m 0755 "${_DL_TMP}/picorecdsp" "$TARGET_BIN"

    rm -rf "$_DL_TMP"
    trap - EXIT

    add_to_backup_list "$TARGET_BIN"
    ok "piCoreCDSP ${PICORECDSP_VERSION} installed at $TARGET_BIN."
}

# ── Step 10: Register startup commands ────────────────────────────────────────
register_startup() {
    echo ""
    echo "=== Step 10: Register startup (bootlocal.sh) ==="

    # snd-aloop: ensure it loads at boot.
    if ! grep -q "snd-aloop" "$BOOTLOCAL" 2>/dev/null; then
        run sh -c "echo 'modprobe snd-aloop' >> '$BOOTLOCAL'"
        ok "Added: modprobe snd-aloop"
    else
        ok "snd-aloop already in bootlocal.sh."
    fi

    # CamillaDSP startup.
    if ! grep -q "camilladsp" "$BOOTLOCAL" 2>/dev/null; then
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would append CamillaDSP startup to ${BOOTLOCAL}"
        else
            cat >> "$BOOTLOCAL" <<EOF

# CamillaDSP v2 — started by piCoreCDSP installer
${INSTALL_BIN}/camilladsp \\
    -p 1234 \\
    -s "${CAMILLA_STATEFILE}" \\
    "${CAMILLA_CONFIG_DIR}/Bypass.yml" \\
    >> /var/log/camilladsp.log 2>&1 &
EOF
        fi
        ok "CamillaDSP startup registered."
    else
        ok "CamillaDSP entry already in bootlocal.sh."
    fi

    # CamillaGUI backend startup.
    if ! grep -q "camillagui" "$BOOTLOCAL" 2>/dev/null; then
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would append CamillaGUI startup to ${BOOTLOCAL}"
        else
            cat >> "$BOOTLOCAL" <<EOF

# CamillaGUI v2 — started by piCoreCDSP installer
python3 ${CAMILLA_GUI_DIR}/main.py \\
    >> /var/log/camillagui.log 2>&1 &
EOF
        fi
        ok "CamillaGUI startup registered."
    else
        ok "CamillaGUI entry already in bootlocal.sh."
    fi

    # piCoreCDSP daemon startup (after CamillaDSP has had a moment to start).
    if ! grep -q "picorecdsp" "$BOOTLOCAL" 2>/dev/null; then
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would append piCoreCDSP startup to ${BOOTLOCAL}"
        else
            cat >> "$BOOTLOCAL" <<EOF

# piCoreCDSP v2 daemon — started by piCoreCDSP installer
sleep 2
${INSTALL_BIN}/picorecdsp >> /var/log/picorecdsp.log 2>&1 &
EOF
        fi
        ok "piCoreCDSP daemon startup registered."
    else
        ok "piCoreCDSP daemon entry already in bootlocal.sh."
    fi

    add_to_backup_list "$BOOTLOCAL"
}

# ── Step 11: Add remaining files to pCP backup list ───────────────────────────
finalize_backup_list() {
    echo ""
    echo "=== Step 11: Finalize pCP backup list ==="
    add_to_backup_list "$FILETOOL_LST"
    ok "Backup list updated."
}

# ── Step 12: Run pCP backup ───────────────────────────────────────────────────
run_backup() {
    echo ""
    echo "=== Step 12: pCP backup ==="
    if command -v filetool.sh >/dev/null 2>&1; then
        run filetool.sh -b
        ok "pCP backup complete."
    else
        warn "filetool.sh not found — skipping backup (not on pCP?)."
    fi
}

# ── Step 13: Reboot prompt ────────────────────────────────────────────────────
prompt_reboot() {
    echo ""
    echo "=== Step 13: Reboot ==="
    echo ""
    echo "  Installation complete.  A reboot is required to:"
    echo "    • Apply the new ALSA configuration."
    echo "    • Start CamillaDSP, CamillaGUI, and piCoreCDSP via bootlocal.sh."
    echo ""
    printf "  Reboot now? [y/N] "
    # shellcheck disable=SC2162
    read ANSWER
    case "$ANSWER" in
        y|Y|yes|YES) run reboot ;;
        *) echo "  Skipping reboot.  Remember to reboot before using piCoreCDSP." ;;
    esac
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
    echo ""
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║  piCoreCDSP v2 Installer                                     ║"
    echo "║  CamillaDSP-native architecture (roadmap §43)                ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
    if [ "$DRY_RUN" -eq 1 ]; then
        echo ""
        echo "  *** DRY-RUN MODE — no changes will be made ***"
    fi
    echo ""

    require_root
    check_platform
    check_aloop
    detect_playback_device
    install_alsa_plug
    install_camilladsp
    install_camillagui
    configure_statefile
    generate_configs
    install_picorecdsp_binary
    register_startup
    finalize_backup_list
    run_backup
    prompt_reboot
}

main "$@"
