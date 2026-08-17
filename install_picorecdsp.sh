#!/bin/sh -e

###############################################################################
# piCoreCDSP v2 — Installer for piCorePlayer
#
# Architecture:
#   - audio path: Producer → pcm.camilladsp → snd-aloop → CamillaDSP → DAC
#   - piCoreCDSP is a single Rust daemon that monitors snd-aloop and drives
#     the CamillaDSP WebSocket control API; no Python/pyalsa dependency.
#   - CamillaDSP and CamillaGUI are downloaded from their official GitHub
#     releases; no build toolchain is required on the piCorePlayer device.
#
# What this installer does:
#   1.  Download and verify the piCoreCDSP daemon, CamillaDSP, and CamillaGUI.
#   2.  Package them as a single piCoreCDSP.tcz Tiny Core extension.
#   3.  Install the pcm.camilladsp ALSA plug definition (/etc/asound.conf).
#   4.  Configure the shared CamillaDSP statefile.
#   5.  Generate Bypass.yml and Null.yml (only if absent; never overwrites).
#   6.  Route piCorePlayer audio output through pcm.camilladsp.
#   7.  Run pCP backup and reboot.
#
# Usage (run as the normal piCorePlayer user "tc"):
#   chmod +x install_picorecdsp.sh
#   ./install_picorecdsp.sh [-k|--keep-downloads] [--playback-device hw:X,Y] [--dry-run]
#
# IMPORTANT: Do NOT run with sudo. Privileged operations use sudo internally.
###############################################################################

###############################################################################
# User check — MUST happen before any modification
###############################################################################

if [ "$(id -u)" -eq 0 ]; then
    echo "ERROR: Do not run this installer with sudo."
    echo
    echo "Run it as the normal piCorePlayer user:"
    echo
    echo "  ./install_picorecdsp.sh"
    echo
    exit 1
fi

if [ "$(id -un)" != "tc" ]; then
    echo "ERROR: This installer must be run as user tc."
    echo "Current user: $(id -un)"
    exit 1
fi

###############################################################################
# Versions
###############################################################################

EXTENSION_NAME="piCoreCDSP"

CDSP_VERSION="v4.1.3"
CAMILLA_GUI_VERSION="v4.1.0"

PICORECDSP_RELEASE_TAG="${PICORECDSP_RELEASE_TAG:-installer-latest}"
PICORECDSP_REPO="urknall/piCoreCDSP"

# Expected SHA256 checksums for the pinned CamillaDSP and CamillaGUI archives.
# These are verified after download to detect corruption or supply-chain tampering.
# Update these values whenever CDSP_VERSION or CAMILLA_GUI_VERSION changes.
CDSP_SHA256_AARCH64="${CDSP_SHA256_AARCH64:-d9a17092923ebfe5d20a770c6b6a7eb2268f9700f999bf604b9db09f518aca5a}"
CDSP_SHA256_ARMV7="${CDSP_SHA256_ARMV7:-dd1af57129e078383e2a1d5dc28cc13f3f02a78dce9247eb7d9232731b8f7609}"
GUI_SHA256_AARCH64="${GUI_SHA256_AARCH64:-9a5415b44dda58478f18de9fd572edf092f659fd5e45cbe8086ff5648dc089d7}"
GUI_SHA256_ARMV7="${GUI_SHA256_ARMV7:-22b89033ebfe1e4d49afd80c0c745bb6bffec19bc2ac2a60279e565524d467d1}"

###############################################################################
# Paths
###############################################################################

BUILD_DIR="/tmp/${EXTENSION_NAME}"
CACHE_DIR="/mnt/mmcblk0p2/tce/${EXTENSION_NAME}-cache"
STAGE_DATA_DIR="/tmp/${EXTENSION_NAME}-data.$$"
ROLLBACK_DIR="/tmp/${EXTENSION_NAME}-rollback.$$"

DATA_DIR="/mnt/mmcblk0p2/tce/camilladsp"
CONFIG_DIR="${DATA_DIR}/configs"
COEFF_DIR="${DATA_DIR}/coeffs"

BYPASS_CONFIG="${CONFIG_DIR}/Bypass.yml"
NULL_CONFIG="${CONFIG_DIR}/Null.yml"
STATEFILE="${DATA_DIR}/camilladsp_statefile.yml"

STAGE_CONFIG_DIR="${STAGE_DATA_DIR}/configs"
STAGE_BYPASS_CONFIG="${STAGE_CONFIG_DIR}/Bypass.yml"
STAGE_NULL_CONFIG="${STAGE_CONFIG_DIR}/Null.yml"

PCP_CONFIG="/usr/local/etc/pcp/pcp.cfg"
PCP_STAGED="/tmp/pcp.cfg.picorecdsp.$$"
ASOUND_STAGED="/tmp/asound.conf.picorecdsp.$$"
TCZ_TMP="/tmp/${EXTENSION_NAME}.tcz"

OPTIONAL_DIR="/etc/sysconfig/tcedir/optional"
ONBOOT_LIST="/etc/sysconfig/tcedir/onboot.lst"
FINAL_TCZ="${OPTIONAL_DIR}/${EXTENSION_NAME}.tcz"

PICORECDSP_BIN="${BUILD_DIR}/usr/local/bin/picorecdsp"

COMMIT_STARTED=false
INSTALL_COMMITTED=false
DATA_DIR_WAS_PRESENT=false

if [ -d "${DATA_DIR}" ]; then
    DATA_DIR_WAS_PRESENT=true
fi

###############################################################################
# Cleanup / rollback
###############################################################################

backup_path() {
    source_path="$1"
    backup_name="$2"

    mkdir -p "${ROLLBACK_DIR}"

    if [ -L "${source_path}" ] || [ -e "${source_path}" ]; then
        sudo cp -pP "${source_path}" "${ROLLBACK_DIR}/${backup_name}"
        echo present > "${ROLLBACK_DIR}/${backup_name}.state"
    else
        echo absent > "${ROLLBACK_DIR}/${backup_name}.state"
    fi
}

restore_path() {
    target_path="$1"
    backup_name="$2"

    state=$(cat "${ROLLBACK_DIR}/${backup_name}.state" 2>/dev/null || echo absent)
    sudo rm -f "${target_path}" 2>/dev/null || true

    if [ "${state}" = present ]; then
        sudo cp -pP "${ROLLBACK_DIR}/${backup_name}" "${target_path}" 2>/dev/null || true
    fi
}

prepare_rollback() {
    rm -rf "${ROLLBACK_DIR}"
    mkdir -p "${ROLLBACK_DIR}"

    backup_path "${PCP_CONFIG}"    pcp.cfg
    backup_path /etc/asound.conf   asound.conf
    backup_path "${ONBOOT_LIST}"   onboot.lst
    backup_path "${BYPASS_CONFIG}" Bypass.yml
    backup_path "${NULL_CONFIG}"   Null.yml
    backup_path "${STATEFILE}"     statefile.yml
}

rollback_install() {
    echo
    echo "ERROR: Installation failed during the commit phase. Rolling back changes..."

    restore_path "${PCP_CONFIG}"  pcp.cfg
    restore_path /etc/asound.conf asound.conf
    restore_path "${ONBOOT_LIST}" onboot.lst

    sudo rm -f "${FINAL_TCZ}" 2>/dev/null || true

    if [ "${DATA_DIR_WAS_PRESENT}" = true ]; then
        restore_path "${BYPASS_CONFIG}" Bypass.yml
        restore_path "${NULL_CONFIG}"   Null.yml
        restore_path "${STATEFILE}"     statefile.yml
    else
        sudo rm -rf "${DATA_DIR}" 2>/dev/null || true
    fi

    # If a failing commit reached `pcp backup`, try to persist the restored
    # state as well. Failure here is non-fatal because rollback must complete.
    pcp backup >/dev/null 2>&1 || true

    echo "Rollback complete. piCorePlayer routing/configuration was restored."
}

cleanup_temp() {
    rm -rf "${BUILD_DIR}" 2>/dev/null || true
    rm -rf "${STAGE_DATA_DIR}" 2>/dev/null || true
    rm -rf "${ROLLBACK_DIR}" 2>/dev/null || true
    rm -f "${PCP_STAGED}" "${ASOUND_STAGED}" 2>/dev/null || true
    rm -f "${TCZ_TMP}" 2>/dev/null || true
}

cleanup() {
    rc=$?
    trap - EXIT HUP INT TERM

    if [ "${COMMIT_STARTED}" = true ] && [ "${INSTALL_COMMITTED}" != true ]; then
        rollback_install
    fi

    cleanup_temp
    exit "${rc}"
}

trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

###############################################################################
# Architecture
###############################################################################

case "$(uname -m)" in
    aarch64)
        architecture="aarch64"
        CDSP_SHA256="${CDSP_SHA256_AARCH64}"
        GUI_SHA256="${GUI_SHA256_AARCH64}"
        ;;
    armv7l|armv7*)
        architecture="armv7"
        CDSP_SHA256="${CDSP_SHA256_ARMV7}"
        GUI_SHA256="${GUI_SHA256_ARMV7}"
        ;;
    *)
        echo "ERROR: Unsupported architecture: $(uname -m)"
        echo "Supported: aarch64, armv7"
        exit 1
        ;;
esac

###############################################################################
# Command line
###############################################################################

show_usage() {
    echo "Usage: $0 [-k|--keep-downloads] [--playback-device hw:X,Y] [--dry-run]"
    echo
    echo "  -k, --keep-downloads         Keep downloaded archives in ${CACHE_DIR}"
    echo "      --playback-device DEV    Physical DAC device (e.g. hw:0,0)"
    echo "      --dry-run                Print actions without executing them"
}

keepDownloads=false
DRY_RUN=false
PLAYBACK_DEVICE_OVERRIDE=""

while [ "$#" -gt 0 ]
do
    case "$1" in
        -k|--keep-downloads)
            keepDownloads=true
            ;;
        --playback-device)
            shift
            if [ "$#" -eq 0 ]; then
                echo "ERROR: --playback-device requires a value."
                show_usage
                exit 1
            fi
            PLAYBACK_DEVICE_OVERRIDE="$1"
            ;;
        --dry-run)
            DRY_RUN=true
            ;;
        -h|--help)
            show_usage
            exit 0
            ;;
        *)
            echo "ERROR: Unknown option: $1"
            show_usage
            exit 1
            ;;
    esac
    shift
done

$keepDownloads && echo "Keeping downloads in ${CACHE_DIR}."

###############################################################################
# Dry-run wrapper
###############################################################################

drun() {
    if $DRY_RUN; then
        echo "  [DRY ]  $*"
    else
        "$@"
    fi
}

###############################################################################
# piCorePlayer extension helpers
###############################################################################

install_if_missing() {
    extension="$1"

    if tce-status -u | grep -q "${extension}"; then
        pcp-load -il "${extension}"
    elif ! tce-status -i | grep -q "${extension}"; then
        pcp-load -wil "${extension}"
    fi
}

install_temporarily_if_missing() {
    extension="$1"

    if tce-status -u | grep -q "${extension}"; then
        pcp-load -il "${extension}"
    elif ! tce-status -i | grep -q "${extension}"; then
        if $keepDownloads; then
            pcp-load -wil "${extension}"
        else
            pcp-load -wil -t /tmp "${extension}"
        fi
    fi
}

download_and_extract_tar_gz() {
    localFileName="$1"
    url="$2"
    expected_sha256="$3"

    echo "Downloading ${url}"

    if $keepDownloads; then
        mkdir -p "${CACHE_DIR}"

        if [ ! -f "${CACHE_DIR}/${localFileName}" ]; then
            # Download atomically: write to .part, verify, then rename.
            wget -O "${CACHE_DIR}/${localFileName}.part" "${url}"
            if ! tar -tzf "${CACHE_DIR}/${localFileName}.part" >/dev/null 2>&1; then
                rm -f "${CACHE_DIR}/${localFileName}.part"
                echo "ERROR: Downloaded archive ${localFileName} is corrupt."
                exit 1
            fi
            mv "${CACHE_DIR}/${localFileName}.part" "${CACHE_DIR}/${localFileName}"
        else
            echo "Using cached ${CACHE_DIR}/${localFileName}"
        fi

        if [ -n "${expected_sha256}" ]; then
            _actual=$(sha256sum "${CACHE_DIR}/${localFileName}" | awk '{print $1}')
            if [ "${_actual}" != "${expected_sha256}" ]; then
                echo "ERROR: SHA256 mismatch for ${localFileName}."
                echo "  Expected: ${expected_sha256}"
                echo "  Got:      ${_actual}"
                rm -f "${CACHE_DIR}/${localFileName}"
                exit 1
            fi
            echo "SHA256 verified: ${localFileName}: ${_actual}"
        fi

        tar -xzf "${CACHE_DIR}/${localFileName}"
    else
        wget -O "${localFileName}.part" "${url}"
        if ! tar -tzf "${localFileName}.part" >/dev/null 2>&1; then
            rm -f "${localFileName}.part"
            echo "ERROR: Downloaded archive ${localFileName} is corrupt."
            exit 1
        fi
        if [ -n "${expected_sha256}" ]; then
            _actual=$(sha256sum "${localFileName}.part" | awk '{print $1}')
            if [ "${_actual}" != "${expected_sha256}" ]; then
                echo "ERROR: SHA256 mismatch for ${localFileName}."
                echo "  Expected: ${expected_sha256}"
                echo "  Got:      ${_actual}"
                rm -f "${localFileName}.part"
                exit 1
            fi
            echo "SHA256 verified: ${localFileName}: ${_actual}"
        fi
        mv "${localFileName}.part" "${localFileName}"
        tar -xzf "${localFileName}"
        rm -f "${localFileName}"
    fi
}

###############################################################################
# Pre-flight
###############################################################################

if [ -f "${FINAL_TCZ}" ]; then
    echo "ERROR: ${EXTENSION_NAME}.tcz is already installed."
    echo
    echo "Uninstall the existing extension and reboot before reinstalling."
    exit 1
fi

# Even when the TCZ is absent (e.g. deleted manually), old piCoreCDSP processes
# may still be running from the previously loaded extension in RAM.  Installing
# over a live runtime risks race conditions and process conflicts.
if [ -e "/usr/local/tce.installed/${EXTENSION_NAME}" ]; then
    echo "ERROR: piCoreCDSP is still loaded in RAM. Reboot first."
    echo
    echo "Reboot piCorePlayer before reinstalling."
    exit 1
fi

_running_procs=""
if pgrep -x picorecdsp >/dev/null 2>&1; then
    _running_procs="${_running_procs} picorecdsp"
fi
if pgrep -x camilladsp >/dev/null 2>&1; then
    _running_procs="${_running_procs} camilladsp"
fi
if pgrep -x camillagui_backend >/dev/null 2>&1; then
    _running_procs="${_running_procs} camillagui_backend"
fi

if [ -n "${_running_procs}" ]; then
    echo "ERROR: Existing piCoreCDSP runtime processes are still running:"
    echo " ${_running_procs}"
    echo
    echo "Reboot piCorePlayer before reinstalling."
    exit 1
fi

requiredSpaceInMB=350

availableSpaceInMB=$(
    /bin/df -m /dev/mmcblk0p2 2>/dev/null |
        awk 'NR==2 { print $4 }'
)

if [ -z "${availableSpaceInMB}" ] ||
   [ "${availableSpaceInMB}" -le "${requiredSpaceInMB}" ]; then
    echo "ERROR: Not enough free space on /dev/mmcblk0p2."
    echo "At least ${requiredSpaceInMB} MB free space is required."
    exit 1
fi

if [ -d "${BUILD_DIR}" ]; then
    echo "ERROR: Build directory ${BUILD_DIR} already exists."
    echo "Remove it or reboot before running the installer again."
    exit 1
fi

mkdir -p "${BUILD_DIR}" "${STAGE_CONFIG_DIR}"

###############################################################################
# Test ALSA Loopback support
###############################################################################

echo "Testing ALSA Loopback support..."

if ! sudo modprobe snd-aloop; then
    echo
    echo "ERROR: snd-aloop could not be loaded."
    echo "This piCorePlayer kernel/image needs ALSA Loopback support."
    exit 1
fi

i=0

while [ "${i}" -lt 10 ]
do
    if grep -q "Loopback" /proc/asound/cards 2>/dev/null; then
        break
    fi

    i=$((i + 1))
    sleep 1
done

if ! grep -q "Loopback" /proc/asound/cards 2>/dev/null; then
    echo
    echo "ERROR: snd-aloop loaded, but the Loopback card did not appear."
    exit 1
fi

echo "ALSA Loopback is available."

###############################################################################
# Resolve the physical playback device
###############################################################################

if [ ! -f "${PCP_CONFIG}" ]; then
    echo "ERROR: piCorePlayer config not found: ${PCP_CONFIG}"
    exit 1
fi

read_pcp_output() {
    awk '
    /^OUTPUT=/ {
        value = $0
        sub(/^OUTPUT=/, "", value)
        sub(/^"/, "", value)
        sub(/"$/, "", value)
        print value
        exit
    }
    ' "${PCP_CONFIG}"
}

is_usable_playback_device() {
    candidate="$1"

    case "${candidate}" in
        ""|null|*Loopback*|*loopback*|picorecdsp|camilladsp)
            return 1
            ;;
    esac

    if printf '%s\n' "${candidate}" | grep -Eqi 'picorecdsp|picoredsp|camilladsp'; then
        return 1
    fi

    return 0
}

if [ -n "${PLAYBACK_DEVICE_OVERRIDE}" ]; then
    PLAYBACK_DEVICE="${PLAYBACK_DEVICE_OVERRIDE}"
    echo "Using provided playback device: ${PLAYBACK_DEVICE}"
else
    PCP_OUTPUT=$(read_pcp_output)

    if is_usable_playback_device "${PCP_OUTPUT}"; then
        PLAYBACK_DEVICE="${PCP_OUTPUT}"
        echo "CamillaDSP playback device: ${PLAYBACK_DEVICE} (from piCorePlayer config)"
    else
        echo
        echo "ERROR: piCorePlayer does not currently have a usable playback output."
        echo "Current OUTPUT: ${PCP_OUTPUT:-<empty>}"
        echo
        echo "Select the real DAC/output in piCorePlayer Squeezelite settings,"
        echo "then run the installer again, or pass --playback-device hw:X,Y."
        exit 1
    fi
fi

###############################################################################
# Download piCoreCDSP daemon
###############################################################################

PICORECDSP_RELEASE_URL="https://github.com/${PICORECDSP_REPO}/releases/download/${PICORECDSP_RELEASE_TAG}/picorecdsp-${architecture}"

echo "Downloading picorecdsp ${PICORECDSP_RELEASE_TAG} for ${architecture}..."

mkdir -p "${BUILD_DIR}/usr/local/bin"

if $keepDownloads; then
    mkdir -p "${CACHE_DIR}"
    CACHED_BIN="${CACHE_DIR}/picorecdsp-${architecture}-${PICORECDSP_RELEASE_TAG}"
    # Never use a stale cache for a mutable rolling tag — always re-download.
    if [ "${PICORECDSP_RELEASE_TAG}" = "installer-latest" ]; then
        echo "Rolling tag ${PICORECDSP_RELEASE_TAG}: skipping cache, re-downloading."
        wget -O "${CACHED_BIN}" "${PICORECDSP_RELEASE_URL}"
    elif [ ! -f "${CACHED_BIN}" ]; then
        wget -O "${CACHED_BIN}" "${PICORECDSP_RELEASE_URL}"
    else
        echo "Using cached ${CACHED_BIN}"
    fi
    cp "${CACHED_BIN}" "${PICORECDSP_BIN}"
else
    wget -O "${PICORECDSP_BIN}" "${PICORECDSP_RELEASE_URL}"
fi

chmod 755 "${PICORECDSP_BIN}"

# Verify SHA256 against the published companion file.
_sha256_tmp="/tmp/picorecdsp-${architecture}.sha256.$$"
wget -O "${_sha256_tmp}" "${PICORECDSP_RELEASE_URL}.sha256" \
    || { echo "ERROR: Failed to download SHA256 for picorecdsp-${architecture}."; exit 1; }
_expected_hash=$(awk '{print $1; exit}' "${_sha256_tmp}")
_actual_hash=$(sha256sum "${PICORECDSP_BIN}" | awk '{print $1}')
rm -f "${_sha256_tmp}"
if [ "${_expected_hash}" != "${_actual_hash}" ]; then
    echo "ERROR: SHA256 mismatch for picorecdsp-${architecture}."
    echo "  Expected: ${_expected_hash}"
    echo "  Got:      ${_actual_hash}"
    exit 1
fi
echo "picorecdsp SHA256 verified: ${_actual_hash}"

if [ ! -x "${PICORECDSP_BIN}" ]; then
    echo "ERROR: picorecdsp binary was not downloaded."
    exit 1
fi

# Verify the binary can actually execute.
"${PICORECDSP_BIN}" --help >/dev/null 2>&1

# Catch missing runtime libraries before the transactional commit.
if command -v ldd >/dev/null 2>&1; then
    if ldd "${PICORECDSP_BIN}" 2>&1 | grep -q 'not found'; then
        echo "ERROR: picorecdsp has unresolved runtime libraries:"
        ldd "${PICORECDSP_BIN}" 2>&1 || true
        exit 1
    fi
fi

###############################################################################
# Stage CamillaDSP configs
###############################################################################

# Bypass.yml — transparent pass-through.
cat > "${STAGE_BYPASS_CONFIG}" <<EOF
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
EOF

# Null.yml — routes audio to /dev/null; diagnostic use only.
cat > "${STAGE_NULL_CONFIG}" <<'EOF'
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
EOF

###############################################################################
# Stage ALSA configuration
###############################################################################

ASOUND_SOURCE=/dev/null
if [ -f /etc/asound.conf ]; then
    ASOUND_SOURCE=/etc/asound.conf
fi

awk '
BEGIN {
    newblock = 0
    oldblock = 0
    found_end = 1
    found_old_end = 1
}

/^# BEGIN piCoreCDSP$/ {
    newblock = 1
    found_end = 0
    next
}

/^# END piCoreCDSP$/ {
    newblock = 0
    found_end = 1
    next
}

/^# For more info about this configuration see: .*alsa_cdsp/ {
    oldblock = 1
    found_old_end = 0
    next
}

oldblock && /^# pcm\.camilladsp$/ {
    oldblock = 0
    found_old_end = 1
    next
}

!newblock && !oldblock {
    print
}

END {
    if (!found_end) {
        print "ERROR: /etc/asound.conf contains a \"# BEGIN piCoreCDSP\" marker without a" \
              " matching \"# END piCoreCDSP\" marker." > "/dev/stderr"
        print "The file may be corrupted. Remove or repair the incomplete block before reinstalling." \
              > "/dev/stderr"
        exit 1
    }
    if (!found_old_end) {
        print "ERROR: /etc/asound.conf contains an old alsa_cdsp block that is missing its" \
              " closing \"# pcm.camilladsp\" marker." > "/dev/stderr"
        print "The file may be corrupted. Remove or repair the incomplete block before reinstalling." \
              > "/dev/stderr"
        exit 1
    }
}
' "${ASOUND_SOURCE}" > "${ASOUND_STAGED}" || {
    echo "ERROR: Failed to process /etc/asound.conf — see message above."
    exit 1
}

cat >> "${ASOUND_STAGED}" <<'ASOUND_BLOCK'

# BEGIN piCoreCDSP

pcm.camilladsp {
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
ASOUND_BLOCK

###############################################################################
# Stage piCorePlayer routing
###############################################################################

cp "${PCP_CONFIG}" "${PCP_STAGED}"

sed 's|^OUTPUT=.*|OUTPUT="picorecdsp"|'                -i "${PCP_STAGED}"
sed 's|^SHAIRPORT_OUT=.*|SHAIRPORT_OUT="picorecdsp"|'  -i "${PCP_STAGED}"
sed 's|^SHAIRPORT_CONTROL=.*|SHAIRPORT_CONTROL=""|'    -i "${PCP_STAGED}"
sed 's|^BT_OUT_DEVICE=.*|BT_OUT_DEVICE="picorecdsp"|'  -i "${PCP_STAGED}"

if ! grep -qx 'OUTPUT="picorecdsp"' "${PCP_STAGED}"; then
    echo "ERROR: Could not stage piCorePlayer OUTPUT routing."
    exit 1
fi

###############################################################################
# Download CamillaDSP
###############################################################################

cd "${BUILD_DIR}/usr/local"

download_and_extract_tar_gz \
    "camilladsp-${CDSP_VERSION}-${architecture}.tar.gz" \
    "https://github.com/HEnquist/camilladsp/releases/download/${CDSP_VERSION}/camilladsp-linux-${architecture}.tar.gz" \
    "${CDSP_SHA256}"

if [ ! -f "${BUILD_DIR}/usr/local/camilladsp" ]; then
    echo "ERROR: CamillaDSP binary was not found after extraction."
    exit 1
fi

chmod 755 "${BUILD_DIR}/usr/local/camilladsp"

if ! "${BUILD_DIR}/usr/local/camilladsp" --help 2>&1 |
    grep -q -- '--wait'
then
    echo "ERROR: Downloaded CamillaDSP does not support --wait."
    exit 1
fi

if ! "${BUILD_DIR}/usr/local/camilladsp" --help 2>&1 |
    grep -q -- '--no_config'
then
    echo "ERROR: Downloaded CamillaDSP does not support --no_config."
    exit 1
fi

_check_out="${BUILD_DIR}/camilladsp-check.$$.log"
if ! "${BUILD_DIR}/usr/local/camilladsp" --check "${STAGE_BYPASS_CONFIG}" \
        >"${_check_out}" 2>&1; then
    echo "ERROR: CamillaDSP config check failed for Bypass.yml:"
    cat "${_check_out}" 2>/dev/null || true
    rm -f "${_check_out}"
    exit 1
fi
rm -f "${_check_out}"

if ! "${BUILD_DIR}/usr/local/camilladsp" --check "${STAGE_NULL_CONFIG}" \
        >"${_check_out}" 2>&1; then
    echo "ERROR: CamillaDSP config check failed for Null.yml:"
    cat "${_check_out}" 2>/dev/null || true
    rm -f "${_check_out}"
    exit 1
fi
rm -f "${_check_out}"

echo "CamillaDSP configuration validation OK."

###############################################################################
# CamillaDSP WebSocket smoke test
#
# Start a temporary CamillaDSP instance in wait/no-config mode and verify
# that its WebSocket port becomes available. This confirms the binary is
# functional on this kernel before committing the full install.
###############################################################################

(
    TEST_PORT=12345
    TEST_LOG="/tmp/picorecdsp-cdsp-ws-test.$$.log"
    TEST_STATEFILE="/tmp/picorecdsp-cdsp-ws-test.$$.state.yml"
    TEST_PID=""

    cleanup_ws_test() {
        if [ -n "${TEST_PID}" ]; then
            kill "${TEST_PID}" >/dev/null 2>&1 || true
            wait "${TEST_PID}" >/dev/null 2>&1 || true
        fi
        rm -f "${TEST_LOG}" "${TEST_STATEFILE}" >/dev/null 2>&1 || true
    }

    trap cleanup_ws_test EXIT HUP INT TERM

    touch "${TEST_STATEFILE}"

    "${BUILD_DIR}/usr/local/camilladsp" \
        --wait \
        --no_config \
        --statefile "${TEST_STATEFILE}" \
        --port "${TEST_PORT}" \
        --address 127.0.0.1 \
        --logfile "${TEST_LOG}" \
        >/dev/null 2>&1 &
    TEST_PID=$!

    i=0
    ws_ready=false
    while [ "${i}" -lt 20 ]
    do
        if ! kill -0 "${TEST_PID}" 2>/dev/null; then
            echo "ERROR: Temporary CamillaDSP exited before WebSocket became available."
            cat "${TEST_LOG}" 2>/dev/null || true
            exit 1
        fi

        # Probe the WebSocket port using nc (available in busybox on piCorePlayer).
        if nc -z 127.0.0.1 "${TEST_PORT}" 2>/dev/null; then
            ws_ready=true
            break
        fi

        i=$((i + 1))
        sleep 1
    done

    if ! $ws_ready; then
        echo "ERROR: CamillaDSP WebSocket port ${TEST_PORT} did not open within 20 s."
        cat "${TEST_LOG}" 2>/dev/null || true
        exit 1
    fi
)

echo "CamillaDSP WebSocket smoke test OK."

###############################################################################
# Download CamillaGUI
###############################################################################

cd "${BUILD_DIR}/usr/local"

download_and_extract_tar_gz \
    "camillagui-${CAMILLA_GUI_VERSION}-${architecture}.tar.gz" \
    "https://github.com/HEnquist/camillagui-backend/releases/download/${CAMILLA_GUI_VERSION}/bundle_linux_${architecture}.tar.gz" \
    "${GUI_SHA256}"

if [ ! -f "${BUILD_DIR}/usr/local/camillagui_backend/camillagui_backend" ]; then
    echo "ERROR: CamillaGUI backend was not found after extraction."
    exit 1
fi

chmod -R 775 "${BUILD_DIR}/usr/local/camillagui_backend"

###############################################################################
# CamillaGUI configuration
###############################################################################

GUI_CONFIG_DIR="${BUILD_DIR}/usr/local/camillagui_backend/_internal/config"

if [ ! -d "${GUI_CONFIG_DIR}" ]; then
    echo "ERROR: CamillaGUI bundle config directory not found:"
    echo "  ${GUI_CONFIG_DIR}"
    exit 1
fi

cat > "${GUI_CONFIG_DIR}/camillagui.yml" <<EOF
camilla_host: "127.0.0.1"
camilla_port: 1234

bind_address: "0.0.0.0"
port: 5000

ssl_certificate: null
ssl_private_key: null

gui_config_file: null

config_dir: "${CONFIG_DIR}"
coeff_dir: "${COEFF_DIR}"
default_config: "${BYPASS_CONFIG}"
statefile_path: "${STATEFILE}"
log_file: "/tmp/camilladsp_rCURRENT.log"

supported_capture_types:
  - "Alsa"

supported_playback_types:
  - "Alsa"
EOF

###############################################################################
# Log-trimmer helper script
###############################################################################

mkdir -p "${BUILD_DIR}/usr/local/bin"

cat > "${BUILD_DIR}/usr/local/bin/picorecdsp-trim-log" <<'EOF'
#!/bin/sh
# Trim a log file to the most recent 256 KB to prevent filling /tmp (RAM).
# Uses copy-truncate so that any process with the file open via ">>" continues
# writing to the same inode rather than to an orphaned unlinked file.
_log="$1"
if [ -f "${_log}" ] && [ "$(wc -c < "${_log}")" -gt 262144 ]; then
    tail -c 262144 "${_log}" > "${_log}.tmp" \
        && cat "${_log}.tmp" > "${_log}" \
        && rm -f "${_log}.tmp" \
        || true
fi
EOF

chmod 755 "${BUILD_DIR}/usr/local/bin/picorecdsp-trim-log"

###############################################################################
# tce.installed boot hook
###############################################################################

mkdir -p "${BUILD_DIR}/usr/local/tce.installed"

cat > "${BUILD_DIR}/usr/local/tce.installed/${EXTENSION_NAME}" <<'TCEEOF'
#!/bin/sh

STATEFILE="/mnt/mmcblk0p2/tce/camilladsp/camilladsp_statefile.yml"
STARTUP_LOG="/tmp/picorecdsp-startup.log"

echo "$(date): piCoreCDSP startup" >> "${STARTUP_LOG}"

###############################################################################
# Load snd-aloop
###############################################################################

if ! modprobe snd-aloop; then
    echo "$(date): unable to load snd-aloop" >> "${STARTUP_LOG}"
    exit 1
fi

i=0
while [ "${i}" -lt 20 ]
do
    if grep -q "Loopback" /proc/asound/cards 2>/dev/null; then
        break
    fi
    i=$((i + 1))
    sleep 1
done

if ! grep -q "Loopback" /proc/asound/cards 2>/dev/null; then
    echo "$(date): Loopback card did not appear" >> "${STARTUP_LOG}"
    exit 1
fi

###############################################################################
# CamillaDSP supervisor
###############################################################################

sudo -u tc sh -c '
exec >> /tmp/camilladsp-supervisor.log 2>&1
_log=/tmp/camilladsp-supervisor.log
while :
do
    /usr/local/bin/picorecdsp-trim-log "${_log}"

    /usr/local/camilladsp \
        --wait \
        --no_config \
        --port 1234 \
        --address 127.0.0.1 \
        --logfile /tmp/camilladsp.log \
        --log_rotate_size 262144 \
        --log_keep_nbr 1 \
        --statefile /mnt/mmcblk0p2/tce/camilladsp/camilladsp_statefile.yml

    rc=$?
    echo "$(date): CamillaDSP exited with ${rc}; restarting" \
        >> /tmp/picorecdsp-startup.log
    sleep 2
done
' &

###############################################################################
# Wait for CamillaDSP WebSocket
###############################################################################

i=0
while [ "${i}" -lt 30 ]
do
    if nc -z 127.0.0.1 1234 2>/dev/null; then
        break
    fi
    i=$((i + 1))
    sleep 1
done

if [ "${i}" -ge 30 ]; then
    echo "$(date): CamillaDSP WebSocket did not become ready" >> "${STARTUP_LOG}"
fi

###############################################################################
# piCoreCDSP daemon supervisor
###############################################################################

sudo -u tc sh -c '
exec >> /tmp/picorecdsp-daemon.log 2>&1
_log=/tmp/picorecdsp-daemon.log
while :
do
    /usr/local/bin/picorecdsp-trim-log "${_log}"

    /usr/local/bin/picorecdsp

    rc=$?
    echo "$(date): piCoreCDSP daemon exited with ${rc}; restarting" \
        >> /tmp/picorecdsp-startup.log
    sleep 2
done
' &

###############################################################################
# CamillaGUI supervisor
###############################################################################

sudo -u tc sh -c '
exec >> /tmp/camillagui-backend.log 2>&1
_log=/tmp/camillagui-backend.log
while :
do
    /usr/local/bin/picorecdsp-trim-log "${_log}"

    /usr/local/camillagui_backend/camillagui_backend

    rc=$?
    echo "$(date): CamillaGUI exited with ${rc}; restarting" \
        >> /tmp/picorecdsp-startup.log
    sleep 2
done
' &

###############################################################################
# Periodic log trimmer (bounds long-running logs even without restarts)
###############################################################################

sudo -u tc sh -c '
exec >> /tmp/picorecdsp-logtrim.log 2>&1
while :
do
    /usr/local/bin/picorecdsp-trim-log /tmp/picorecdsp-daemon.log
    /usr/local/bin/picorecdsp-trim-log /tmp/camillagui-backend.log
    /usr/local/bin/picorecdsp-trim-log /tmp/camilladsp-supervisor.log
    sleep 60
done
' &

exit 0
TCEEOF

chmod 775 "${BUILD_DIR}/usr/local/tce.installed/${EXTENSION_NAME}"

###############################################################################
# Build Tiny Core extension
###############################################################################

install_temporarily_if_missing squashfs-tools

rm -f "${TCZ_TMP}"

mksquashfs "${BUILD_DIR}" "${TCZ_TMP}" -noappend

if [ ! -s "${TCZ_TMP}" ]; then
    echo "ERROR: TCZ build did not produce a valid file."
    exit 1
fi

# piCoreCDSP has no Tiny Core runtime dependency beyond libraries already
# present in the piCorePlayer ALSA stack. No .tcz.dep is installed.

###############################################################################
# Pre-commit validation
###############################################################################

if [ ! -s "${STAGE_BYPASS_CONFIG}" ] || \
   [ ! -s "${STAGE_NULL_CONFIG}" ]   || \
   [ ! -s "${ASOUND_STAGED}" ]       || \
   [ ! -s "${PCP_STAGED}" ]; then
    echo "ERROR: One or more staged installation files are missing."
    exit 1
fi

if [ ! -x "${BUILD_DIR}/usr/local/camilladsp" ] || \
   [ ! -x "${BUILD_DIR}/usr/local/camillagui_backend/camillagui_backend" ] || \
   [ ! -x "${PICORECDSP_BIN}" ]; then
    echo "ERROR: Staged runtime is incomplete."
    exit 1
fi

echo
echo "All downloads and validations completed successfully."
echo "Committing piCoreCDSP changes to piCorePlayer..."

###############################################################################
# Transactional commit
###############################################################################

if $DRY_RUN; then
    echo "  [DRY ]  Would commit: install ${FINAL_TCZ}, update onboot.lst, asound.conf, pcp.cfg, configs, statefile."
    INSTALL_COMMITTED=true
    COMMIT_STARTED=false
    cleanup_temp
    trap - EXIT HUP INT TERM
    echo
    echo "Dry run complete — no changes written."
    exit 0
fi

prepare_rollback
COMMIT_STARTED=true

# Persistent CamillaDSP data is changed only now, after all validation passed.
sudo mkdir -p "${DATA_DIR}" "${CONFIG_DIR}" "${COEFF_DIR}"

if [ ! -f "${BYPASS_CONFIG}" ]; then
    sudo cp -f "${STAGE_BYPASS_CONFIG}" "${BYPASS_CONFIG}"
fi

if [ ! -f "${NULL_CONFIG}" ]; then
    sudo cp -f "${STAGE_NULL_CONFIG}" "${NULL_CONFIG}"
fi

# Statefile — create a placeholder so CamillaDSP can populate it on first run.
if [ ! -f "${STATEFILE}" ]; then
    sudo touch "${STATEFILE}"
fi

# Install extension before routing live audio to it.
sudo mv -f "${TCZ_TMP}" "${FINAL_TCZ}"

if ! grep -qx "${EXTENSION_NAME}.tcz" "${ONBOOT_LIST}" 2>/dev/null; then
    echo "${EXTENSION_NAME}.tcz" | sudo tee -a "${ONBOOT_LIST}" >/dev/null
fi

# Commit ALSA PCM definition.
sudo touch /etc/asound.conf
sudo chmod 664 /etc/asound.conf
sudo chown root:staff /etc/asound.conf
sudo tee /etc/asound.conf < "${ASOUND_STAGED}" >/dev/null

# Route pCP sources to pcm.camilladsp LAST, when extension/config are in place.
sudo tee "${PCP_CONFIG}" < "${PCP_STAGED}" >/dev/null

# Apply ownership and permissions only to the directories and files this
# installer created or modified.
for _d in "${DATA_DIR}" "${CONFIG_DIR}" "${COEFF_DIR}"; do
    sudo chown tc:staff "${_d}" 2>/dev/null || true
    sudo chmod u+rwx,g+rwx "${_d}" 2>/dev/null || true
done
for _f in "${BYPASS_CONFIG}" "${NULL_CONFIG}" "${STATEFILE}"; do
    [ -f "${_f}" ] || continue
    sudo chown tc:staff "${_f}" 2>/dev/null || true
    sudo chmod u+rw,g+rw "${_f}" 2>/dev/null || true
done

# Persist only after the entire commit completed successfully.
pcp backup

INSTALL_COMMITTED=true

# From this point onward cleanup must not roll back the installed system.
COMMIT_STARTED=false
cleanup_temp
trap - EXIT HUP INT TERM

###############################################################################
# Summary
###############################################################################

echo
echo "Installation complete."
echo
echo "Audio path:"
echo
echo "  Squeezelite / AirPlay / Bluetooth"
echo "                |"
echo "                v"
echo "         pcm.camilladsp"
echo "                |"
echo "                v"
echo "        hw:Loopback,1,0"
echo "                |"
echo "            snd-aloop"
echo "                |"
echo "                v"
echo "        hw:Loopback,0,0"
echo "                |"
echo "                v"
echo "           CamillaDSP"
echo "                |"
echo "                v"
echo "               DAC"
echo "  (${PLAYBACK_DEVICE})"
echo
echo "CamillaGUI after reboot:"
echo "  http://pcp.local:5000"
echo
echo "Useful logs:"
echo "  /tmp/picorecdsp-startup.log"
echo "  /tmp/picorecdsp-daemon.log"
echo "  /tmp/camilladsp-supervisor.log"
echo "  /tmp/camilladsp.log"
echo "  /tmp/camillagui-backend.log"
echo

###############################################################################
# Reboot
###############################################################################

pcp reboot
