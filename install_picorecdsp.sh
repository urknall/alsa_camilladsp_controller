#!/usr/bin/env sh
# piCoreCDSP v2 — Installer for piCorePlayer (roadmap §43, Gate 11)
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
#   6.  Install the pinned CamillaGUI backend (pre-built native bundle, no Python).
#   7.  Configure the shared native statefile.
#   8.  Generate Bypass.yml and Null.yml (only if absent; never overwrites).
#   9.  Install the piCoreCDSP v2 Rust daemon binary.
#  10.  Register all components for pCP startup (bootlocal.sh) with supervisor loops.
#  11.  Route piCorePlayer audio output to pcm.picorecdsp.
#  12.  Add installed files to pCP backup list.
#  13.  Run pCP backup.
#  14.  Prompt for reboot.
#
# What this installer does NOT do:
#   ✗  No reinstall / migration path.
#   ✗  No backend menu or backend switcher.
#   ✗  No overwriting of existing user configs.
#   ✗  No Squeezelite parameter management.
#   ✗  No runtime YAML creation.
#   ✗  No shadow config file.
#   ✗  No Python, no pip, no tce-load for Python.
#
# Usage:
#   wget https://github.com/urknall/piCoreCDSP/releases/download/vX.Y.Z/install_picorecdsp.sh
#   chmod +x install_picorecdsp.sh
#   ./install_picorecdsp.sh [--playback-device hw:X,Y] [--dry-run]
#
# Pinned stack (set at Gate 12 hardware validation — update these variables):
#   CAMILLA_VERSION      CamillaDSP release tag (e.g. "v2.0.3")
#   CAMILLA_GUI_VERSION  CamillaGUI release tag
#   PICORECDSP_VERSION   piCoreCDSP release tag
#   CAMILLA_SHA256_AARCH64 / CAMILLA_SHA256_ARMV7
#   GUI_SHA256_AARCH64   / GUI_SHA256_ARMV7
#   PICORECDSP_SHA256_AARCH64 / PICORECDSP_SHA256_ARMV7

set -eu

# ── Version pins (Gate 12 fills these in after hardware validation) ───────────
CAMILLA_VERSION="${CAMILLA_VERSION:-GATE12_PIN_REQUIRED}"
CAMILLA_GUI_VERSION="${CAMILLA_GUI_VERSION:-GATE12_PIN_REQUIRED}"
PICORECDSP_VERSION="${PICORECDSP_VERSION:-GATE12_PIN_REQUIRED}"

# SHA256 checksums for the pinned download archives.
# Update these whenever the version pins change.
CAMILLA_SHA256_AARCH64="${CAMILLA_SHA256_AARCH64:-}"
CAMILLA_SHA256_ARMV7="${CAMILLA_SHA256_ARMV7:-}"
GUI_SHA256_AARCH64="${GUI_SHA256_AARCH64:-}"
GUI_SHA256_ARMV7="${GUI_SHA256_ARMV7:-}"
PICORECDSP_SHA256_AARCH64="${PICORECDSP_SHA256_AARCH64:-}"
PICORECDSP_SHA256_ARMV7="${PICORECDSP_SHA256_ARMV7:-}"

# ── Installation paths ────────────────────────────────────────────────────────
INSTALL_BIN="/usr/local/bin"
CAMILLA_CONFIG_DIR="/mnt/mmcblk0p2/tce/camilladsp/configs"
CAMILLA_COEFF_DIR="/mnt/mmcblk0p2/tce/camilladsp/coeffs"
CAMILLA_DATA_DIR="/mnt/mmcblk0p2/tce/camilladsp"
CAMILLA_STATEFILE="${CAMILLA_DATA_DIR}/camilladsp_statefile.yml"
CAMILLA_GUI_DIR="/usr/local/camillagui_backend"
ASOUND_CONF="/etc/asound.conf"
BOOTLOCAL="/opt/bootlocal.sh"
PCP_CONFIG="/usr/local/etc/pcp/pcp.cfg"
FILETOOL_LST="/home/tc/.filetool.lst"

# Required free space in MB before downloading
REQUIRED_SPACE_MB=350

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
    [ ! -f "$1" ] || [ ! -s "$1" ]
}

add_to_backup_list() {
    _entry="$1"
    if ! grep -qxF "$_entry" "$FILETOOL_LST" 2>/dev/null; then
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would add $_entry to pCP backup list."
        else
            printf '%s\n' "$_entry" >> "$FILETOOL_LST"
            info "Added $_entry to pCP backup list."
        fi
    fi
}

# Download url → local path, verify SHA256 (optional), atomic (.part rename).
download_file() {
    _url="$1"
    _dest="$2"
    _expected_sha256="${3:-}"

    _part="${_dest}.part"
    info "Downloading $(basename "$_dest") ..."
    run wget -q -O "$_part" "$_url" || abort "Download failed: $_url"

    if [ -n "$_expected_sha256" ] && [ "$DRY_RUN" -eq 0 ]; then
        _actual=$(sha256sum "$_part" | awk '{print $1}')
        if [ "$_actual" != "$_expected_sha256" ]; then
            rm -f "$_part"
            abort "SHA256 mismatch for $(basename "$_dest"). Expected: $_expected_sha256  Got: $_actual"
        fi
        info "SHA256 verified: $(basename "$_dest")"
    fi

    run mv "$_part" "$_dest"
}

# ── Step 1: Platform check ────────────────────────────────────────────────────
check_platform() {
    echo ""
    echo "=== Step 1: Platform check ==="
    if [ ! -f /etc/os-release ] && [ ! -f /usr/share/doc/pcp/version ]; then
        if ! grep -qi 'picore\|tinycore\|picoreplayer' /etc/issue 2>/dev/null &&
           ! grep -qi 'picore\|tinycore\|picoreplayer' /proc/version 2>/dev/null; then
            abort "Not running on piCorePlayer / TinyCore Linux. Aborting."
        fi
    fi
    ok "piCorePlayer detected."
}

# ── Step 2: Pre-flight checks ─────────────────────────────────────────────────
preflight_checks() {
    echo ""
    echo "=== Step 2: Pre-flight checks ==="

    # snd-aloop
    if grep -q "^snd.aloop\|^snd_aloop" /proc/modules 2>/dev/null; then
        ok "snd-aloop module is loaded."
    elif modprobe snd-aloop 2>/dev/null; then
        ok "snd-aloop loaded successfully."
    else
        abort "snd-aloop is not available in this kernel. piCoreCDSP v2 requires snd-aloop."
    fi

    # Loopback card appearance
    _i=0
    while [ "$_i" -lt 10 ]; do
        grep -q "Loopback" /proc/asound/cards 2>/dev/null && break
        _i=$((_i + 1))
        sleep 1
    done
    grep -q "Loopback" /proc/asound/cards 2>/dev/null \
        || abort "snd-aloop loaded but Loopback card did not appear in /proc/asound/cards."
    ok "ALSA Loopback card is visible."

    # Running process conflicts
    _procs=""
    pgrep -x camilladsp       >/dev/null 2>&1 && _procs="$_procs camilladsp"
    pgrep -x camillagui_backend >/dev/null 2>&1 && _procs="$_procs camillagui_backend"
    pgrep -x picorecdsp       >/dev/null 2>&1 && _procs="$_procs picorecdsp"
    if [ -n "$_procs" ]; then
        abort "Existing runtime processes are still running:$_procs — reboot before installing."
    fi
    ok "No conflicting processes running."

    # Disk space
    if command -v df >/dev/null 2>&1; then
        _avail=$(df -m /dev/mmcblk0p2 2>/dev/null | awk 'NR==2{print $4}' || echo "")
        if [ -n "$_avail" ] && [ "$_avail" -lt "$REQUIRED_SPACE_MB" ]; then
            abort "Not enough free space on /dev/mmcblk0p2. Need ${REQUIRED_SPACE_MB} MB, have ${_avail} MB."
        fi
        [ -n "$_avail" ] && ok "Disk space OK (${_avail} MB free)."
    fi

    # Version pins
    for _var in CAMILLA_VERSION CAMILLA_GUI_VERSION PICORECDSP_VERSION; do
        _val=$(eval echo "\$$_var")
        if [ "$_val" = "GATE12_PIN_REQUIRED" ]; then
            abort "$_var is not set. Hardware validation (Gate 12) must pin the version before this installer can run. Set: export $_var=v<x.y.z>"
        fi
    done
    ok "Version pins set: CamillaDSP=${CAMILLA_VERSION}  CamillaGUI=${CAMILLA_GUI_VERSION}  piCoreCDSP=${PICORECDSP_VERSION}"
}

# ── Step 3: Playback device detection ────────────────────────────────────────
detect_playback_device() {
    echo ""
    echo "=== Step 3: Physical playback device detection ==="

    if [ -n "$PLAYBACK_DEVICE" ]; then
        ok "Using provided playback device: $PLAYBACK_DEVICE"
        return
    fi

    # Try to recover from existing pcp.cfg (reinstall: OUTPUT may already be
    # pcm.picorecdsp, so we look at the last-known device file too).
    if [ -f "$PCP_CONFIG" ]; then
        _pcp_out=$(awk '/^OUTPUT=/{v=$0; sub(/^OUTPUT=/,"",v); gsub(/"/,"",v); print v; exit}' "$PCP_CONFIG")
        case "$_pcp_out" in
            ""|*picorecdsp*|*camilladsp*|*Loopback*|*loopback*) ;;
            *) PLAYBACK_DEVICE="$_pcp_out"; ok "Detected playback device from pcp.cfg: $PLAYBACK_DEVICE"; return ;;
        esac
    fi

    # Auto-detect from aplay
    if command -v aplay >/dev/null 2>&1; then
        _detected=$(aplay -l 2>/dev/null \
            | grep "^card " \
            | grep -v -i "loopback" \
            | head -1 \
            | sed 's/card \([0-9]*\): .*, device \([0-9]*\):.*/hw:\1,\2/' \
            || true)
        if [ -n "$_detected" ]; then
            PLAYBACK_DEVICE="$_detected"
            ok "Detected playback device: $PLAYBACK_DEVICE"
            return
        fi
    fi

    warn "No non-loopback playback device detected. Falling back to hw:0,0."
    warn "Pass --playback-device hw:X,Y if this is wrong."
    PLAYBACK_DEVICE="hw:0,0"
}

# ── Step 4: Install pcm.picorecdsp ALSA plug ─────────────────────────────────
install_alsa_plug() {
    echo ""
    echo "=== Step 4: Install pcm.picorecdsp ALSA plug ==="

    if [ -f "$ASOUND_CONF" ] && grep -q "pcm.picorecdsp" "$ASOUND_CONF"; then
        ok "$ASOUND_CONF already contains pcm.picorecdsp — skipping."
        return
    fi

    # Strip any existing piCoreCDSP block (idempotent reinstall safety),
    # then append the current canonical definition.
    if [ "$DRY_RUN" -eq 0 ]; then
        _tmp=$(mktemp)
        trap 'rm -f "$_tmp"' EXIT

        # Strip old block if present, pass rest through unchanged.
        if [ -f "$ASOUND_CONF" ]; then
            awk '
                /^# BEGIN piCoreCDSP$/ { skip=1; next }
                /^# END piCoreCDSP$/   { skip=0; next }
                !skip { print }
            ' "$ASOUND_CONF" > "$_tmp"
        fi

        cat >> "$_tmp" <<'PCM_BLOCK'

# BEGIN piCoreCDSP
# pcm.picorecdsp ALSA plug — generated by piCoreCDSP installer.
# CANONICAL definition: src/source/alsa_loopback.rs (CANONICAL_ASOUND_CONF).
# If the slave PCM changes, update both places and re-run contract tests.
#
# Contract invariants:
#   format   = S32_LE   (producers honour or auto-convert via plug)
#   channels = 2        (stereo only)
#   rate     = unchanged (no plug-level resampling; rate negotiated end-to-end)

pcm.picorecdsp {
    type plug
    slave {
        pcm "hw:Loopback,1,0"
        format S32_LE
        channels 2
        rate unchanged
    }
    hint {
        show on
        description "piCoreCDSP ALSA Loopback input"
    }
}
# END piCoreCDSP
PCM_BLOCK

        touch "$ASOUND_CONF"
        chmod 664 "$ASOUND_CONF"
        cp "$_tmp" "$ASOUND_CONF"
        rm -f "$_tmp"
        trap - EXIT
    else
        echo "  [DRY ]  Would write pcm.picorecdsp block to $ASOUND_CONF"
    fi

    add_to_backup_list "$ASOUND_CONF"
    ok "pcm.picorecdsp installed in $ASOUND_CONF."
}

# ── Step 5: Install CamillaDSP ────────────────────────────────────────────────
install_camilladsp() {
    echo ""
    echo "=== Step 5: Install CamillaDSP ${CAMILLA_VERSION} ==="

    TARGET_BIN="${INSTALL_BIN}/camilladsp"

    if [ -f "$TARGET_BIN" ]; then
        _ver=$("$TARGET_BIN" --version 2>/dev/null | head -1 || echo "unknown")
        abort "CamillaDSP already installed ($_ver). Fresh-install-only. Remove $TARGET_BIN manually to reinstall."
    fi

    _arch=$(uname -m)
    case "$_arch" in
        aarch64|arm64) _rel_arch="aarch64-unknown-linux-musl"; _sha="$CAMILLA_SHA256_AARCH64" ;;
        armv7l)        _rel_arch="armv7-unknown-linux-musleabihf"; _sha="$CAMILLA_SHA256_ARMV7" ;;
        x86_64)        _rel_arch="x86_64-unknown-linux-musl"; _sha="" ;;
        *) abort "Unsupported architecture: $_arch" ;;
    esac

    _url="https://github.com/HEnquist/camilladsp/releases/download/${CAMILLA_VERSION}/camilladsp-${CAMILLA_VERSION}-${_rel_arch}.tar.gz"
    _tmp=$(mktemp -d)
    trap 'rm -rf "$_tmp"' EXIT

    download_file "$_url" "${_tmp}/camilladsp.tar.gz" "$_sha"

    if [ "$DRY_RUN" -eq 0 ]; then
        tar -xzf "${_tmp}/camilladsp.tar.gz" -C "$_tmp"
        install -m 0755 "${_tmp}/camilladsp" "$TARGET_BIN"
    else
        echo "  [DRY ]  Would extract and install camilladsp to $TARGET_BIN"
    fi

    rm -rf "$_tmp"
    trap - EXIT

    add_to_backup_list "$TARGET_BIN"
    ok "CamillaDSP ${CAMILLA_VERSION} installed at $TARGET_BIN."
}

# ── Step 6: Install CamillaGUI (pre-built native binary bundle, no Python) ───
install_camillagui() {
    echo ""
    echo "=== Step 6: Install CamillaGUI ${CAMILLA_GUI_VERSION} (native binary bundle) ==="

    if [ -d "$CAMILLA_GUI_DIR" ]; then
        abort "CamillaGUI directory already exists at $CAMILLA_GUI_DIR. Fresh-install-only. Remove it manually to reinstall."
    fi

    _arch=$(uname -m)
    case "$_arch" in
        aarch64|arm64) _bundle_arch="aarch64"; _sha="$GUI_SHA256_AARCH64" ;;
        armv7l)        _bundle_arch="armv7";   _sha="$GUI_SHA256_ARMV7" ;;
        x86_64)        _bundle_arch="x86_64";  _sha="" ;;
        *) abort "Unsupported architecture: $_arch" ;;
    esac

    # CamillaGUI ships a self-contained PyInstaller bundle — native binary,
    # no Python runtime required on the target system.
    _url="https://github.com/HEnquist/camillagui-backend/releases/download/${CAMILLA_GUI_VERSION}/bundle_linux_${_bundle_arch}.tar.gz"
    _tmp=$(mktemp -d)
    trap 'rm -rf "$_tmp"' EXIT

    download_file "$_url" "${_tmp}/camillagui.tar.gz" "$_sha"

    if [ "$DRY_RUN" -eq 0 ]; then
        run mkdir -p "$CAMILLA_GUI_DIR"
        tar -xzf "${_tmp}/camillagui.tar.gz" -C "$CAMILLA_GUI_DIR"

        # Verify the binary is present after extraction.
        if [ ! -f "${CAMILLA_GUI_DIR}/camillagui_backend" ]; then
            abort "CamillaGUI binary not found after extraction at ${CAMILLA_GUI_DIR}/camillagui_backend."
        fi
        chmod -R 775 "$CAMILLA_GUI_DIR"
    else
        echo "  [DRY ]  Would extract bundle to $CAMILLA_GUI_DIR"
    fi

    rm -rf "$_tmp"
    trap - EXIT

    add_to_backup_list "$CAMILLA_GUI_DIR"
    ok "CamillaGUI ${CAMILLA_GUI_VERSION} installed at $CAMILLA_GUI_DIR (no Python required)."
}

# ── Step 7: Configure shared native statefile ─────────────────────────────────
configure_statefile() {
    echo ""
    echo "=== Step 7: Configure shared native statefile ==="

    run mkdir -p "$CAMILLA_DATA_DIR" "$CAMILLA_CONFIG_DIR" "$CAMILLA_COEFF_DIR"

    if [ -f "$CAMILLA_STATEFILE" ]; then
        ok "Statefile already exists at $CAMILLA_STATEFILE — not overwriting."
    else
        run touch "$CAMILLA_STATEFILE"
        add_to_backup_list "$CAMILLA_STATEFILE"
        ok "Statefile placeholder created at $CAMILLA_STATEFILE."
        info "(CamillaDSP will populate this on first run.)"
    fi

    # Write CamillaGUI config into the bundle's config directory.
    # The bundle expects its config at _internal/config/camillagui.yml.
    _gui_conf_dir="${CAMILLA_GUI_DIR}/_internal/config"
    _gui_conf="${_gui_conf_dir}/camillagui.yml"

    if [ -f "$_gui_conf" ] && [ "$DRY_RUN" -eq 0 ]; then
        ok "CamillaGUI config already present at $_gui_conf — skipping."
        return
    fi

    if [ "$DRY_RUN" -eq 1 ]; then
        echo "  [DRY ]  Would write ${_gui_conf}"
        return
    fi

    mkdir -p "$_gui_conf_dir"
    cat > "$_gui_conf" <<EOF
# CamillaGUI config — generated by piCoreCDSP installer.
camilla_host: "127.0.0.1"
camilla_port: 1234

bind_address: "0.0.0.0"
port: 5000

ssl_certificate: null
ssl_private_key: null

config_dir: "${CAMILLA_CONFIG_DIR}"
coeff_dir: "${CAMILLA_COEFF_DIR}"
default_config: "${CAMILLA_CONFIG_DIR}/Bypass.yml"
statefile_path: "${CAMILLA_STATEFILE}"
log_file: "/tmp/camilladsp_rCURRENT.log"

supported_capture_types:
  - "Alsa"

supported_playback_types:
  - "Alsa"
EOF
    ok "CamillaGUI config written to $_gui_conf."
}

# ── Step 8: Generate Bypass.yml and Null.yml ──────────────────────────────────
generate_configs() {
    echo ""
    echo "=== Step 8: Generate Bypass.yml / Null.yml ==="

    add_to_backup_list "$CAMILLA_CONFIG_DIR"

    _bypass="${CAMILLA_CONFIG_DIR}/Bypass.yml"
    _null="${CAMILLA_CONFIG_DIR}/Null.yml"

    if [ -f "$_bypass" ]; then
        ok "Bypass.yml already exists — not overwriting (user-owned)."
    else
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would write ${_bypass}"
        else
            cat > "$_bypass" <<EOF
# Bypass.yml — generated by piCoreCDSP v2 installer.
# User-owned after installation: the installer never overwrites this file.
#
# CamillaDSP version: ${CAMILLA_VERSION}
# Generated for playback device: ${PLAYBACK_DEVICE}

devices:
  samplerate: 44100
  chunksize: 1024
  queuelimit: 4
  silence_threshold: 0
  silence_timeout: 0.0
  stop_on_inactive: true
  capture:
    type: Alsa
    channels: 2
    device: "hw:Loopback,0,0"
    format: S32LE
  playback:
    type: Alsa
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

    if [ -f "$_null" ]; then
        ok "Null.yml already exists — not overwriting (user-owned)."
    else
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would write ${_null}"
        else
            cat > "$_null" <<EOF
# Null.yml — generated by piCoreCDSP v2 installer.
# Routes audio to /dev/null (silence). Diagnostic use only.
#
# CamillaDSP version: ${CAMILLA_VERSION}

devices:
  samplerate: 44100
  chunksize: 1024
  queuelimit: 4
  silence_threshold: 0
  silence_timeout: 0.0
  stop_on_inactive: true
  capture:
    type: Alsa
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
    echo "=== Step 9: Install piCoreCDSP daemon ${PICORECDSP_VERSION} ==="

    TARGET_BIN="${INSTALL_BIN}/picorecdsp"

    if [ -f "$TARGET_BIN" ]; then
        abort "piCoreCDSP already installed at $TARGET_BIN. Fresh-install-only. Remove it manually to reinstall."
    fi

    _arch=$(uname -m)
    case "$_arch" in
        aarch64|arm64) _rel_arch="aarch64-unknown-linux-musl"; _sha="$PICORECDSP_SHA256_AARCH64" ;;
        armv7l)        _rel_arch="armv7-unknown-linux-musleabihf"; _sha="$PICORECDSP_SHA256_ARMV7" ;;
        x86_64)        _rel_arch="x86_64-unknown-linux-musl"; _sha="" ;;
        *) abort "Unsupported architecture: $_arch" ;;
    esac

    _url="https://github.com/urknall/piCoreCDSP/releases/download/${PICORECDSP_VERSION}/picorecdsp-${PICORECDSP_VERSION}-${_rel_arch}.tar.gz"
    _tmp=$(mktemp -d)
    trap 'rm -rf "$_tmp"' EXIT

    download_file "$_url" "${_tmp}/picorecdsp.tar.gz" "$_sha"

    if [ "$DRY_RUN" -eq 0 ]; then
        tar -xzf "${_tmp}/picorecdsp.tar.gz" -C "$_tmp"
        install -m 0755 "${_tmp}/picorecdsp" "$TARGET_BIN"
    else
        echo "  [DRY ]  Would extract and install picorecdsp to $TARGET_BIN"
    fi

    rm -rf "$_tmp"
    trap - EXIT

    add_to_backup_list "$TARGET_BIN"
    ok "piCoreCDSP ${PICORECDSP_VERSION} installed at $TARGET_BIN."
}

# ── Step 10: Smoke-test binaries before committing system config ───────────────
smoke_test_binaries() {
    echo ""
    echo "=== Step 10: Binary smoke tests ==="

    if [ "$DRY_RUN" -eq 1 ]; then
        echo "  [DRY ]  Skipping smoke tests in dry-run mode."
        return
    fi

    # CamillaDSP: validate both generated configs.
    "${INSTALL_BIN}/camilladsp" --check "${CAMILLA_CONFIG_DIR}/Bypass.yml" \
        || abort "CamillaDSP config validation failed for Bypass.yml."
    ok "CamillaDSP validated Bypass.yml."

    "${INSTALL_BIN}/camilladsp" --check "${CAMILLA_CONFIG_DIR}/Null.yml" \
        || abort "CamillaDSP config validation failed for Null.yml."
    ok "CamillaDSP validated Null.yml."

    # CamillaGUI binary executes.
    "${CAMILLA_GUI_DIR}/camillagui_backend" --help >/dev/null 2>&1 || true
    [ -x "${CAMILLA_GUI_DIR}/camillagui_backend" ] \
        || abort "CamillaGUI binary is not executable at ${CAMILLA_GUI_DIR}/camillagui_backend."
    ok "CamillaGUI binary is executable."

    # piCoreCDSP daemon executes.
    "${INSTALL_BIN}/picorecdsp" --help >/dev/null 2>&1 || true
    [ -x "${INSTALL_BIN}/picorecdsp" ] \
        || abort "piCoreCDSP binary is not executable at ${INSTALL_BIN}/picorecdsp."
    ok "piCoreCDSP binary is executable."
}

# ── Step 11: Register startup with supervisor loops ───────────────────────────
register_startup() {
    echo ""
    echo "=== Step 11: Register startup (bootlocal.sh) ==="

    # snd-aloop
    if ! grep -q "snd-aloop" "$BOOTLOCAL" 2>/dev/null; then
        run sh -c "printf 'modprobe snd-aloop\n' >> '$BOOTLOCAL'"
        ok "Added: modprobe snd-aloop"
    else
        ok "snd-aloop already in bootlocal.sh."
    fi

    # CamillaDSP supervisor loop
    if ! grep -q "camilladsp" "$BOOTLOCAL" 2>/dev/null; then
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would append CamillaDSP supervisor to ${BOOTLOCAL}"
        else
            cat >> "$BOOTLOCAL" <<EOF

# CamillaDSP v2 supervisor — started by piCoreCDSP installer
sudo -u tc sh -c '
  exec >> /tmp/camilladsp-supervisor.log 2>&1
  _log=/tmp/camilladsp-supervisor.log
  while :; do
    ${INSTALL_BIN}/camilladsp \\\\
      --wait \\\\
      --no_config \\\\
      -p 1234 \\\\
      -s "${CAMILLA_STATEFILE}" \\\\
      --logfile /tmp/camilladsp.log \\\\
      --log_rotate_size 262144 \\\\
      --log_keep_nbr 1
    echo "\$(date): CamillaDSP exited \$?; restarting" >> /tmp/picorecdsp-startup.log
    sleep 2
  done
' &
EOF
        fi
        ok "CamillaDSP supervisor registered."
    else
        ok "CamillaDSP entry already in bootlocal.sh."
    fi

    # CamillaGUI supervisor loop (native binary — no Python)
    if ! grep -q "camillagui" "$BOOTLOCAL" 2>/dev/null; then
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would append CamillaGUI supervisor to ${BOOTLOCAL}"
        else
            cat >> "$BOOTLOCAL" <<EOF

# CamillaGUI v2 supervisor — started by piCoreCDSP installer
sudo -u tc sh -c '
  exec >> /tmp/camillagui-backend.log 2>&1
  _log=/tmp/camillagui-backend.log
  while :; do
    ${CAMILLA_GUI_DIR}/camillagui_backend
    echo "\$(date): CamillaGUI exited \$?; restarting" >> /tmp/picorecdsp-startup.log
    sleep 2
  done
' &
EOF
        fi
        ok "CamillaGUI supervisor registered."
    else
        ok "CamillaGUI entry already in bootlocal.sh."
    fi

    # piCoreCDSP daemon supervisor loop
    if ! grep -q "picorecdsp" "$BOOTLOCAL" 2>/dev/null; then
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "  [DRY ]  Would append piCoreCDSP supervisor to ${BOOTLOCAL}"
        else
            cat >> "$BOOTLOCAL" <<EOF

# piCoreCDSP v2 daemon supervisor — started by piCoreCDSP installer
sudo -u tc sh -c '
  exec >> /tmp/picorecdsp-daemon.log 2>&1
  _log=/tmp/picorecdsp-daemon.log
  sleep 3
  while :; do
    ${INSTALL_BIN}/picorecdsp
    echo "\$(date): piCoreCDSP daemon exited \$?; restarting" >> /tmp/picorecdsp-startup.log
    sleep 2
  done
' &
EOF
        fi
        ok "piCoreCDSP daemon supervisor registered."
    else
        ok "piCoreCDSP daemon entry already in bootlocal.sh."
    fi

    add_to_backup_list "$BOOTLOCAL"
}

# ── Step 12: Route piCorePlayer audio output to pcm.picorecdsp ───────────────
route_pcp_output() {
    echo ""
    echo "=== Step 12: Route piCorePlayer output to pcm.picorecdsp ==="

    if [ ! -f "$PCP_CONFIG" ]; then
        warn "pcp.cfg not found at $PCP_CONFIG — skipping OUTPUT routing."
        warn "Set OUTPUT=\"picorecdsp\" in pcp.cfg manually before rebooting."
        return
    fi

    if grep -q 'OUTPUT="picorecdsp"' "$PCP_CONFIG" 2>/dev/null; then
        ok "pcp.cfg OUTPUT already set to picorecdsp — skipping."
        return
    fi

    if [ "$DRY_RUN" -eq 1 ]; then
        echo "  [DRY ]  Would set OUTPUT=\"picorecdsp\" in $PCP_CONFIG"
        return
    fi

    # Stage the change atomically.
    _staged=$(mktemp)
    trap 'rm -f "$_staged"' EXIT
    sed 's|^OUTPUT=.*|OUTPUT="picorecdsp"|' "$PCP_CONFIG" > "$_staged"

    if ! grep -q 'OUTPUT="picorecdsp"' "$_staged"; then
        # OUTPUT line was absent — append it.
        printf 'OUTPUT="picorecdsp"\n' >> "$_staged"
    fi

    cp "$_staged" "$PCP_CONFIG"
    rm -f "$_staged"
    trap - EXIT

    add_to_backup_list "$PCP_CONFIG"
    ok "pcp.cfg OUTPUT set to picorecdsp."
}

# ── Step 13: Finalize pCP backup list ─────────────────────────────────────────
finalize_backup_list() {
    echo ""
    echo "=== Step 13: Finalize pCP backup list ==="
    add_to_backup_list "$FILETOOL_LST"
    add_to_backup_list "$CAMILLA_DATA_DIR"
    ok "Backup list updated."
}

# ── Step 14: Run pCP backup ───────────────────────────────────────────────────
run_backup() {
    echo ""
    echo "=== Step 14: pCP backup ==="
    if command -v pcp >/dev/null 2>&1; then
        run pcp backup
        ok "pCP backup complete."
    elif command -v filetool.sh >/dev/null 2>&1; then
        run filetool.sh -b
        ok "pCP backup complete (filetool.sh)."
    else
        warn "Neither pcp nor filetool.sh found — skipping backup."
    fi
}

# ── Step 15: Reboot prompt ────────────────────────────────────────────────────
prompt_reboot() {
    echo ""
    echo "=== Step 15: Reboot ==="
    echo ""
    echo "  Installation complete. A reboot is required to:"
    echo "    • Apply the new ALSA configuration."
    echo "    • Start CamillaDSP, CamillaGUI, and piCoreCDSP via bootlocal.sh."
    echo "    • Apply the new piCorePlayer output routing."
    echo ""
    echo "  Audio path after reboot:"
    echo "    Squeezelite / AirPlay → pcm.picorecdsp → hw:Loopback,1,0"
    echo "                         → snd-aloop → hw:Loopback,0,0"
    echo "                         → CamillaDSP → ${PLAYBACK_DEVICE}"
    echo ""
    echo "  CamillaGUI: http://pcp.local:5000"
    echo ""
    echo "  Logs:"
    echo "    /tmp/picorecdsp-startup.log"
    echo "    /tmp/camilladsp-supervisor.log"
    echo "    /tmp/camillagui-backend.log"
    echo "    /tmp/picorecdsp-daemon.log"
    echo ""

    if [ "$DRY_RUN" -eq 1 ]; then
        echo "  [DRY ]  Dry-run complete. No changes were made."
        return
    fi

    # Non-interactive (piped stdin): auto-reboot.
    if [ ! -t 0 ]; then
        info "Non-interactive mode: rebooting now."
        if command -v pcp >/dev/null 2>&1; then
            pcp reboot
        else
            reboot
        fi
        return
    fi

    printf "  Reboot now? [y/N] "
    # shellcheck disable=SC2162
    read ANSWER
    case "$ANSWER" in
        y|Y|yes|YES)
            if command -v pcp >/dev/null 2>&1; then
                pcp reboot
            else
                reboot
            fi
            ;;
        *)
            echo "  Skipping reboot. Remember to reboot before using piCoreCDSP."
            ;;
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
    preflight_checks
    detect_playback_device
    install_alsa_plug
    install_camilladsp
    install_camillagui
    configure_statefile
    generate_configs
    install_picorecdsp_binary
    smoke_test_binaries
    register_startup
    route_pcp_output
    finalize_backup_list
    run_backup
    prompt_reboot
}

main "$@"
