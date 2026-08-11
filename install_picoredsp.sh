#!/bin/sh -e

###############################################################################
# piCoreCDSP - snd-aloop + Rust CamillaDSP controller
#
# Architecture:
#   - audio stays on the proven ALSA snd-aloop path;
#   - the Python/pyalsa/pyCamillaDSP runtime is replaced by one Rust binary;
#   - the Rust controller source lives in the GitHub repository and pre-built
#     aarch64/armv7 binaries are downloaded from GitHub Releases; no Rust
#     toolchain is required on the piCorePlayer device.
#
# The controller follows the Linux --adapt behavior of
# HEnquist/camilladsp-controller, plus piCoreDSP-specific extensions:
#   * re-read CamillaGUI's active_config.yml symlink on every adaptation;
#   * adapt the actual initial rate/format/channels before first start;
#   * re-read the complete wave format after CaptureFormatChange.
#
# The controller stays out of the PCM data path: ALSA/snd-aloop carries
# audio, while this binary only monitors ALSA controls and drives CamillaDSP's
# documented WebSocket control API.
#
# IMPORTANT:
#   Run this installer as the normal piCorePlayer user "tc":
#
#       ./install.sh
#
#   Do NOT run:
#
#       sudo ./install.sh
#
# Privileged operations are performed with sudo only where required.
###############################################################################

###############################################################################
# User check - MUST happen before any modification
###############################################################################

if [ "$(id -u)" -eq 0 ]; then
    echo "ERROR: Do not run this installer with sudo."
    echo
    echo "Run it as the normal piCorePlayer user:"
    echo
    echo "  ./install.sh"
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
OUTPUT_PCM_NAME="picoredsp"

CDSP_VERSION="v4.1.3"
CAMILLA_GUI_VERSION="v4.1.0"

CONTROLLER_RELEASE_TAG="installer-latest"
CONTROLLER_REPO="urknall/alsa_camilladsp_controller"

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

DEFAULT_CONFIG="${DATA_DIR}/default_config.yml"
BYPASS_CONFIG="${CONFIG_DIR}/Bypass.yml"
NULL_CONFIG="${CONFIG_DIR}/Null.yml"
STATEFILE="${DATA_DIR}/camilladsp_statefile.yml"
ACTIVE_CONFIG_LINK="${DATA_DIR}/active_config.yml"
PLAYBACK_DEVICE_FILE="${DATA_DIR}/playback_device.txt"

STAGE_CONFIG_DIR="${STAGE_DATA_DIR}/configs"
STAGE_DEFAULT_CONFIG="${STAGE_DATA_DIR}/default_config.yml"
STAGE_BYPASS_CONFIG="${STAGE_CONFIG_DIR}/Bypass.yml"
STAGE_NULL_CONFIG="${STAGE_CONFIG_DIR}/Null.yml"
STAGE_STATEFILE="${STAGE_DATA_DIR}/camilladsp_statefile.yml"
STAGE_ACTIVE_CONFIG_LINK="${STAGE_DATA_DIR}/active_config.yml"
STAGE_PLAYBACK_DEVICE_FILE="${STAGE_DATA_DIR}/playback_device.txt"

PCP_CONFIG="/usr/local/etc/pcp/pcp.cfg"
PCP_STAGED="/tmp/pcp.cfg.picoredsp.$$"
ASOUND_STAGED="/tmp/asound.conf.picoredsp.$$"
DEP_STAGED="/tmp/${EXTENSION_NAME}.tcz.dep.$$"
TCZ_TMP="/tmp/${EXTENSION_NAME}.tcz"

OPTIONAL_DIR="/etc/sysconfig/tcedir/optional"
ONBOOT_LIST="/etc/sysconfig/tcedir/onboot.lst"
FINAL_TCZ="${OPTIONAL_DIR}/${EXTENSION_NAME}.tcz"
FINAL_DEP="${OPTIONAL_DIR}/${EXTENSION_NAME}.tcz.dep"

RUST_RUNTIME_BIN="${BUILD_DIR}/usr/local/bin/picoredsp-controller"

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

    backup_path "${PCP_CONFIG}" pcp.cfg
    backup_path /etc/asound.conf asound.conf
    backup_path "${ONBOOT_LIST}" onboot.lst
    backup_path "${FINAL_DEP}" extension.dep

    backup_path "${DEFAULT_CONFIG}" default_config.yml
    backup_path "${BYPASS_CONFIG}" Bypass.yml
    backup_path "${NULL_CONFIG}" Null.yml
    backup_path "${STATEFILE}" statefile.yml
    backup_path "${ACTIVE_CONFIG_LINK}" active_config.yml
    backup_path "${PLAYBACK_DEVICE_FILE}" playback_device.txt
}

rollback_install() {
    echo
    echo "ERROR: Installation failed during the commit phase. Rolling back changes..."

    restore_path "${PCP_CONFIG}" pcp.cfg
    restore_path /etc/asound.conf asound.conf
    restore_path "${ONBOOT_LIST}" onboot.lst
    restore_path "${FINAL_DEP}" extension.dep

    sudo rm -f "${FINAL_TCZ}" 2>/dev/null || true

    if [ "${DATA_DIR_WAS_PRESENT}" = true ]; then
        restore_path "${DEFAULT_CONFIG}" default_config.yml
        restore_path "${BYPASS_CONFIG}" Bypass.yml
        restore_path "${NULL_CONFIG}" Null.yml
        restore_path "${STATEFILE}" statefile.yml
        restore_path "${ACTIVE_CONFIG_LINK}" active_config.yml
        restore_path "${PLAYBACK_DEVICE_FILE}" playback_device.txt
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
    rm -f "${PCP_STAGED}" "${ASOUND_STAGED}" "${DEP_STAGED}" 2>/dev/null || true
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
        ;;
    armv7l|armv7*)
        architecture="armv7"
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
    echo "Usage: $0 [-k|--keep-downloads]"
    echo
    echo "  -k, --keep-downloads   Keep downloaded archives in ${CACHE_DIR}"
}

keepDownloads=false

for parameter in "$@"
do
    case "${parameter}" in
        -k|--keep-downloads)
            keepDownloads=true
            ;;
        -h|--help)
            show_usage
            exit 0
            ;;
        *)
            show_usage
            exit 1
            ;;
    esac
done

$keepDownloads && echo "Keeping downloads in ${CACHE_DIR}."

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

    echo "Downloading ${url}"

    if $keepDownloads; then
        mkdir -p "${CACHE_DIR}"

        if [ ! -f "${CACHE_DIR}/${localFileName}" ]; then
            wget -O "${CACHE_DIR}/${localFileName}" "${url}"
        else
            echo "Using cached ${CACHE_DIR}/${localFileName}"
        fi

        tar -xzf "${CACHE_DIR}/${localFileName}"
    else
        wget -O "${localFileName}" "${url}"
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

requiredSpaceInMB=350

availableSpaceInMB=$(
    /bin/df -m /dev/mmcblk0p2 |
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
# Download picoredsp-controller
###############################################################################

# Map the piCorePlayer architecture identifier to the binary name used in
# GitHub release artefacts.
case "${architecture}" in
    aarch64) controller_arch="aarch64" ;;
    armv7)   controller_arch="armv7"   ;;
esac

CONTROLLER_RELEASE_URL="https://github.com/${CONTROLLER_REPO}/releases/download/${CONTROLLER_RELEASE_TAG}/picoredsp-controller-${controller_arch}"

echo "Downloading picoredsp-controller ${CONTROLLER_RELEASE_TAG} for ${controller_arch}..."

mkdir -p "${BUILD_DIR}/usr/local/bin"

if $keepDownloads; then
    mkdir -p "${CACHE_DIR}"
    CACHED_CONTROLLER="${CACHE_DIR}/picoredsp-controller-${controller_arch}-${CONTROLLER_RELEASE_TAG}"
    # Never use a stale cache for a mutable rolling tag — always re-download.
    if [ "${CONTROLLER_RELEASE_TAG}" = "installer-latest" ]; then
        echo "Rolling tag ${CONTROLLER_RELEASE_TAG}: skipping cache, re-downloading."
        wget -O "${CACHED_CONTROLLER}" "${CONTROLLER_RELEASE_URL}"
    elif [ ! -f "${CACHED_CONTROLLER}" ]; then
        wget -O "${CACHED_CONTROLLER}" "${CONTROLLER_RELEASE_URL}"
    else
        echo "Using cached ${CACHED_CONTROLLER}"
    fi
    cp "${CACHED_CONTROLLER}" "${RUST_RUNTIME_BIN}"
else
    wget -O "${RUST_RUNTIME_BIN}" "${CONTROLLER_RELEASE_URL}"
fi

chmod 755 "${RUST_RUNTIME_BIN}"

# Verify SHA256 checksum against the published .sha256 file before executing
# the binary or passing it through --help or --probe.
_sha256_tmp="/tmp/picoredsp-controller-${controller_arch}.sha256.$$"
wget -O "${_sha256_tmp}" "${CONTROLLER_RELEASE_URL}.sha256" \
    || { echo "ERROR: Failed to download SHA256 checksum for picoredsp-controller-${controller_arch}."; exit 1; }
_expected_hash=$(awk '{print $1; exit}' "${_sha256_tmp}")
_actual_hash=$(sha256sum "${RUST_RUNTIME_BIN}" | awk '{print $1}')
rm -f "${_sha256_tmp}"
if [ "${_expected_hash}" != "${_actual_hash}" ]; then
    echo "ERROR: SHA256 mismatch for picoredsp-controller-${controller_arch}."
    echo "  Expected: ${_expected_hash}"
    echo "  Got:      ${_actual_hash}"
    exit 1
fi
echo "picoredsp-controller SHA256 verified: ${_actual_hash}"

if [ ! -x "${RUST_RUNTIME_BIN}" ]; then
    echo "ERROR: picoredsp-controller binary was not downloaded."
    exit 1
fi

# Verify the binary can actually execute.
"${RUST_RUNTIME_BIN}" --help >/dev/null

# Catch missing runtime libraries before the transactional commit.
if command -v ldd >/dev/null 2>&1; then
    if ldd "${RUST_RUNTIME_BIN}" 2>&1 | grep -q 'not found'; then
        echo "ERROR: picoredsp-controller has unresolved runtime libraries:"
        ldd "${RUST_RUNTIME_BIN}" 2>&1 || true
        exit 1
    fi
fi

# Verify the binary can open the exact snd-aloop HCTL device used at runtime,
# find the expected PCM controls, and read their values on this kernel.
echo "Probing snd-aloop controls with picoredsp-controller..."
"${RUST_RUNTIME_BIN}" --probe --device hw:Loopback,0


###############################################################################
# Resolve the real CamillaDSP playback device
#
# First install:
#   Preserve the physical output currently selected for Squeezelite.
#
# Reinstall:
#   Squeezelite is normally already routed to pcm.picoredsp. In that case,
#   recover the real output from the currently selected CamillaDSP config.
#   Fall back to Bypass.yml and finally to the last-known output saved by a
#   previous successful install. Never guess from ALSA card numbering.
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

read_statefile_config_path() {
    statefile="$1"
    [ -f "${statefile}" ] || return 1

    awk '
    /^[[:space:]]*config_path:[[:space:]]*/ {
        value = $0
        sub(/^[[:space:]]*config_path:[[:space:]]*/, "", value)
        sub(/^"/, "", value)
        sub(/"$/, "", value)
        sub(/^\047/, "", value)
        sub(/\047$/, "", value)
        print value
        exit
    }
    ' "${statefile}"
}

is_usable_playback_device() {
    candidate="$1"

    case "${candidate}" in
        ""|null|*Loopback*|*loopback*|"${OUTPUT_PCM_NAME}"|camilladsp)
            return 1
            ;;
    esac

    if printf '%s\n' "${candidate}" | grep -Eqi 'picoredsp|pcm\.picoredsp|camilladsp'; then
        return 1
    fi

    return 0
}

PCP_OUTPUT=$(read_pcp_output)
PLAYBACK_DEVICE=""
PLAYBACK_SOURCE=""
INSTALL_MODE="first-install"

# EXISTING_INSTALL is true when any piece of a previous piCoreDSP installation
# is detected on disk, regardless of how Squeezelite is currently routed.
# Volume/mute and the active config symlink are preserved whenever
# EXISTING_INSTALL=true so that a user who temporarily switched Squeezelite
# back to the physical DAC does not lose their settings.
EXISTING_INSTALL=false
if [ -f "${STATEFILE}" ] || \
   [ -L "${ACTIVE_CONFIG_LINK}" ] || \
   [ -f "${PLAYBACK_DEVICE_FILE}" ]; then
    EXISTING_INSTALL=true
fi

if is_usable_playback_device "${PCP_OUTPUT}"; then
    PLAYBACK_DEVICE="${PCP_OUTPUT}"
    PLAYBACK_SOURCE="piCorePlayer Squeezelite output"
else
    case "${PCP_OUTPUT}" in
        "${OUTPUT_PCM_NAME}"|camilladsp)
            INSTALL_MODE="reinstall"

            ACTIVE_CONFIG_TARGET=""
            if [ -L "${ACTIVE_CONFIG_LINK}" ]; then
                ACTIVE_CONFIG_TARGET=$(readlink -f "${ACTIVE_CONFIG_LINK}" 2>/dev/null || true)
            fi

            if [ -z "${ACTIVE_CONFIG_TARGET}" ]; then
                ACTIVE_CONFIG_TARGET=$(read_statefile_config_path "${STATEFILE}" 2>/dev/null || true)
            fi

            if [ -n "${ACTIVE_CONFIG_TARGET}" ]; then
                candidate=$("${RUST_RUNTIME_BIN}" --get-playback-device "${ACTIVE_CONFIG_TARGET}" 2>/dev/null || true)
                if is_usable_playback_device "${candidate}"; then
                    PLAYBACK_DEVICE="${candidate}"
                    PLAYBACK_SOURCE="active CamillaDSP config (${ACTIVE_CONFIG_TARGET})"
                fi
            fi

            if [ -z "${PLAYBACK_DEVICE}" ]; then
                candidate=$("${RUST_RUNTIME_BIN}" --get-playback-device "${BYPASS_CONFIG}" 2>/dev/null || true)
                if is_usable_playback_device "${candidate}"; then
                    PLAYBACK_DEVICE="${candidate}"
                    PLAYBACK_SOURCE="existing Bypass.yml"
                fi
            fi

            if [ -z "${PLAYBACK_DEVICE}" ] && [ -f "${PLAYBACK_DEVICE_FILE}" ]; then
                IFS= read -r candidate < "${PLAYBACK_DEVICE_FILE}" || true
                if is_usable_playback_device "${candidate}"; then
                    PLAYBACK_DEVICE="${candidate}"
                    PLAYBACK_SOURCE="last-known piCoreDSP playback output"
                fi
            fi

            if [ -z "${PLAYBACK_DEVICE}" ]; then
                echo
                echo "ERROR: piCorePlayer is already routed to '${PCP_OUTPUT}', but the"
                echo "physical CamillaDSP playback device could not be recovered."
                echo
                echo "Select the real DAC/output temporarily in Squeezelite Settings,"
                echo "then run the installer again."
                exit 1
            fi
            ;;

        *)
            echo
            echo "ERROR: piCorePlayer does not currently have a usable playback output."
            echo "Current OUTPUT: ${PCP_OUTPUT:-<empty>}"
            echo "Select the real DAC/output in piCorePlayer before the first install."
            exit 1
            ;;
    esac
fi

echo "Install mode: ${INSTALL_MODE}"
echo "CamillaDSP playback device: ${PLAYBACK_DEVICE}"
echo "Playback device source: ${PLAYBACK_SOURCE}"

###############################################################################
# Stage CamillaDSP data (no persistent changes yet)
###############################################################################

# Leave ALSA sample formats automatic. With snd-aloop, the playback side fixes
# the format that the capture side must use. CamillaDSP can select that format.
"${RUST_RUNTIME_BIN}" --make-bypass \
    --playback-device "${PLAYBACK_DEVICE}" \
    --output "${STAGE_BYPASS_CONFIG}"

cp "${STAGE_BYPASS_CONFIG}" "${STAGE_DEFAULT_CONFIG}"

cat > "${STAGE_NULL_CONFIG}" <<'EOF'
devices:
  samplerate: 44100
  chunksize: 2048
  queuelimit: 4
  enable_rate_adjust: true

  capture:
    type: Alsa
    channels: 2
    device: "hw:Loopback,0,0"
    stop_on_inactive: true

  playback:
    type: Alsa
    channels: 2
    device: "null"

filters: {}
mixers: {}
pipeline: []
processors: {}

title: 'Null'
description: |
  Diagnostic-only configuration.
  Audio is captured from snd-aloop and intentionally discarded.
  Do not select this configuration when testing audible output.
EOF

# The statefile contains FINAL runtime paths even though it is staged here.
# On reinstall, preserve any existing volume/mute values so a user's current
# speaker levels are not silently reset to 0 dB.
_stage_mute_block="- false
- false
- false
- false
- false"
_stage_volume_block="- 0.0
- 0.0
- 0.0
- 0.0
- 0.0"

if $EXISTING_INSTALL && [ -f "${STATEFILE}" ]; then
    _extracted_mute=$(awk '
        /^mute:/   { section="mute";   next }
        /^volume:/ { section="volume"; next }
        /^[a-z_]/  { section="" }
        section == "mute" && /^- / { print; next }
    ' "${STATEFILE}" | head -5)
    _extracted_volume=$(awk '
        /^mute:/   { section="mute";   next }
        /^volume:/ { section="volume"; next }
        /^[a-z_]/  { section="" }
        section == "volume" && /^- / { print; next }
    ' "${STATEFILE}" | head -5)
    if [ -n "${_extracted_mute}" ] && [ -n "${_extracted_volume}" ]; then
        _stage_mute_block="${_extracted_mute}"
        _stage_volume_block="${_extracted_volume}"
    fi
fi

cat > "${STAGE_STATEFILE}" <<EOF
config_path: ${BYPASS_CONFIG}

mute:
${_stage_mute_block}

volume:
${_stage_volume_block}
EOF

ln -sfn "${STAGE_BYPASS_CONFIG}" "${STAGE_ACTIVE_CONFIG_LINK}"
printf '%s\n' "${PLAYBACK_DEVICE}" > "${STAGE_PLAYBACK_DEVICE_FILE}"

###############################################################################
# Stage ALSA configuration (no write to /etc yet)
###############################################################################

ASOUND_SOURCE=/dev/null
if [ -f /etc/asound.conf ]; then
    ASOUND_SOURCE=/etc/asound.conf
fi

awk '
BEGIN {
    newblock = 0
    oldblock = 0
}

/^# BEGIN piCoreDSP$/ {
    newblock = 1
    next
}

/^# END piCoreDSP$/ {
    newblock = 0
    next
}

/^# For more info about this configuration see: .*alsa_cdsp/ {
    oldblock = 1
    next
}

oldblock && /^# pcm\.camilladsp$/ {
    oldblock = 0
    next
}

!newblock && !oldblock {
    print
}
' "${ASOUND_SOURCE}" > "${ASOUND_STAGED}"

cat >> "${ASOUND_STAGED}" <<'EOF'

# BEGIN piCoreDSP

pcm.picoredsp {
    type plug

    slave {
        pcm "hw:Loopback,1,0"
        channels 2
    }

    hint {
        show on
        description "piCoreDSP ALSA Loopback"
    }
}

# END piCoreDSP
EOF

###############################################################################
# Stage piCorePlayer routing (no write to pcp.cfg yet)
###############################################################################

cp "${PCP_CONFIG}" "${PCP_STAGED}"

sed 's|^OUTPUT=.*|OUTPUT="picoredsp"|' -i "${PCP_STAGED}"
sed 's|^SHAIRPORT_OUT=.*|SHAIRPORT_OUT="picoredsp"|' -i "${PCP_STAGED}"
sed 's|^SHAIRPORT_CONTROL=.*|SHAIRPORT_CONTROL=""|' -i "${PCP_STAGED}"
sed 's|^BT_OUT_DEVICE=.*|BT_OUT_DEVICE="picoredsp"|' -i "${PCP_STAGED}"

if ! grep -qx 'OUTPUT="picoredsp"' "${PCP_STAGED}"; then
    echo "ERROR: Could not stage piCorePlayer OUTPUT routing."
    exit 1
fi

# Verify piCoreDSP-specific adaptation behavior on the ACTUAL staged Bypass
# config: symlink/file parsing, rate update and channel validation.
RUST_ADAPTED_TEST="/tmp/picoredsp-adapted.$$.yml"
"${RUST_RUNTIME_BIN}" \
    --adapt-check \
    --adapt "${STAGE_BYPASS_CONFIG}" \
    --rate 48000 \
    --format S32_LE \
    --channels 2 \
    > "${RUST_ADAPTED_TEST}"

if ! grep -Eq '^[[:space:]]*samplerate:[[:space:]]*48000[[:space:]]*$' "${RUST_ADAPTED_TEST}"; then
    echo "ERROR: picoredsp-controller did not adapt samplerate to 48000."
    exit 1
fi

if [ "$("${RUST_RUNTIME_BIN}" --get-playback-device "${RUST_ADAPTED_TEST}" 2>/dev/null || true)" != "${PLAYBACK_DEVICE}" ]; then
    echo "ERROR: Controller adaptation did not preserve the selected playback device."
    exit 1
fi

rm -f "${RUST_ADAPTED_TEST}"

echo "picoredsp-controller ${CONTROLLER_RELEASE_TAG} download and ALSA probe OK."

###############################################################################
# Download CamillaDSP
###############################################################################

cd "${BUILD_DIR}/usr/local"

download_and_extract_tar_gz     "camilladsp-${CDSP_VERSION}-${architecture}.tar.gz"     "https://github.com/HEnquist/camilladsp/releases/download/${CDSP_VERSION}/camilladsp-linux-${architecture}.tar.gz"

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

"${BUILD_DIR}/usr/local/camilladsp" --check "${STAGE_BYPASS_CONFIG}" >/dev/null
"${BUILD_DIR}/usr/local/camilladsp" --check "${STAGE_NULL_CONFIG}" >/dev/null

echo "CamillaDSP configuration validation OK."


###############################################################################
# Rust <-> CamillaDSP WebSocket integration smoke test
###############################################################################

# Run a temporary CamillaDSP in wait/no-config mode. This tests the actual
# tungstenite client against CamillaDSP 4.1.3 without opening the real DAC.
(
    TEST_PORT=12345
    TEST_LOG="/tmp/picoredsp-camilladsp-ws-test.$$.log"
    TEST_PID=""

    cleanup_ws_test() {
        if [ -n "${TEST_PID}" ]; then
            kill "${TEST_PID}" >/dev/null 2>&1 || true
            wait "${TEST_PID}" >/dev/null 2>&1 || true
        fi
        rm -f "${TEST_LOG}" >/dev/null 2>&1 || true
    }

    trap cleanup_ws_test EXIT HUP INT TERM

    "${BUILD_DIR}/usr/local/camilladsp" \
        --wait \
        --no_config \
        --port "${TEST_PORT}" \
        --address 127.0.0.1 \
        --logfile "${TEST_LOG}" \
        >/dev/null 2>&1 &
    TEST_PID=$!

    i=0
    while [ "${i}" -lt 20 ]
    do
        if "${RUST_RUNTIME_BIN}" \
            --ws-check \
            --host 127.0.0.1 \
            --port "${TEST_PORT}" \
            >/dev/null 2>&1
        then
            break
        fi
        i=$((i + 1))
        sleep 1
    done

    if [ "${i}" -ge 20 ]; then
        echo "ERROR: Rust controller could not establish a CamillaDSP WebSocket session."
        cat "${TEST_LOG}" 2>/dev/null || true
        exit 1
    fi

    # Validate an adapted real staged config over the documented WebSocket
    # command protocol. ValidateConfig does not start audio processing.
    "${RUST_RUNTIME_BIN}" \
        --ws-validate \
        --host 127.0.0.1 \
        --port "${TEST_PORT}" \
        --adapt "${STAGE_BYPASS_CONFIG}" \
        --rate 48000 \
        --format S32_LE \
        --channels 2 \
        >/dev/null
)

echo "Rust controller WebSocket integration with CamillaDSP OK."

###############################################################################
# Download CamillaGUI
###############################################################################

cd "${BUILD_DIR}/usr/local"

download_and_extract_tar_gz     "camillagui-${CAMILLA_GUI_VERSION}-${architecture}.tar.gz"     "https://github.com/HEnquist/camillagui-backend/releases/download/${CAMILLA_GUI_VERSION}/bundle_linux_${architecture}.tar.gz"

if [ ! -f "${BUILD_DIR}/usr/local/camillagui_backend/camillagui_backend" ]; then
    echo "ERROR: CamillaGUI backend was not found after extraction."
    exit 1
fi

chmod -R 775     "${BUILD_DIR}/usr/local/camillagui_backend"

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
default_config: "${DEFAULT_CONFIG}"
statefile_path: "${STATEFILE}"
log_file: "/tmp/camilladsp.log"

on_set_active_config: "ln -sfn {} ${ACTIVE_CONFIG_LINK}"
on_get_active_config: "readlink -f ${ACTIVE_CONFIG_LINK}"

supported_capture_types:
  - "Alsa"

supported_playback_types:
  - "Alsa"
EOF

###############################################################################
# Boot script
###############################################################################

mkdir -p "${BUILD_DIR}/usr/local/tce.installed"

cat > "${BUILD_DIR}/usr/local/tce.installed/${EXTENSION_NAME}" <<'EOF'
#!/bin/sh

CONTROLLER="/usr/local/bin/picoredsp-controller"
ACTIVE_CONFIG="/mnt/mmcblk0p2/tce/camilladsp/active_config.yml"
STATEFILE="/mnt/mmcblk0p2/tce/camilladsp/camilladsp_statefile.yml"

STARTUP_LOG="/tmp/picoredsp-startup.log"

echo "$(date): piCoreDSP startup" >> "${STARTUP_LOG}"
echo "$(date): active config: $(readlink -f "${ACTIVE_CONFIG}" 2>/dev/null)" >> "${STARTUP_LOG}"

###############################################################################
# ALSA Loopback
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
while :
do
    /usr/local/camilladsp         --wait         --no_config         --port 1234         --address 127.0.0.1         --logfile /tmp/camilladsp.log         --statefile /mnt/mmcblk0p2/tce/camilladsp/camilladsp_statefile.yml

    rc=$?

    echo "$(date): CamillaDSP exited with ${rc}; restarting"         >> /tmp/picoredsp-startup.log

    sleep 2
done
' >> /tmp/camilladsp-supervisor.log 2>&1 &

###############################################################################
# Wait for CamillaDSP websocket
###############################################################################

i=0

while [ "${i}" -lt 30 ]
do
    if sudo -u tc "${CONTROLLER}" \
        --ws-check \
        --host 127.0.0.1 \
        --port 1234 \
        >/dev/null 2>&1
    then
        break
    fi

    i=$((i + 1))
    sleep 1
done

if [ "${i}" -ge 30 ]; then
    echo "$(date): CamillaDSP websocket did not become ready"         >> "${STARTUP_LOG}"
fi

###############################################################################
# Controller supervisor
###############################################################################

sudo -u tc sh -c '
while :
do
    /usr/local/bin/picoredsp-controller \
        --host 127.0.0.1 \
        --port 1234 \
        --device hw:Loopback,0 \
        --adapt /mnt/mmcblk0p2/tce/camilladsp/active_config.yml \
        --log-level INFO

    rc=$?

    echo "$(date): Rust piCoreDSP controller exited with ${rc}; restarting" \
        >> /tmp/picoredsp-startup.log

    sleep 2
done
' >> /tmp/picoredsp-controller.log 2>&1 &

###############################################################################
# CamillaGUI
###############################################################################

sudo -u tc     /usr/local/camillagui_backend/camillagui_backend     >> /tmp/camillagui-backend.log     2>&1 &

exit 0
EOF

chmod 775     "${BUILD_DIR}/usr/local/tce.installed/${EXTENSION_NAME}"

###############################################################################
# Build Tiny Core extension
###############################################################################

install_temporarily_if_missing squashfs-tools

rm -f "${TCZ_TMP}"

mksquashfs     "${BUILD_DIR}"     "${TCZ_TMP}"     -noappend

if [ ! -s "${TCZ_TMP}" ]; then
    echo "ERROR: TCZ build did not produce a valid file."
    exit 1
fi

# The Rust controller has no Tiny Core runtime dependency beyond libraries
# already present in piCorePlayer's ALSA stack. No .tcz.dep is installed.

###############################################################################
# Final pre-commit validation
###############################################################################

if [ ! -s "${STAGE_DEFAULT_CONFIG}" ] ||
   [ ! -s "${STAGE_BYPASS_CONFIG}" ] ||
   [ ! -s "${STAGE_NULL_CONFIG}" ] ||
   [ ! -s "${STAGE_STATEFILE}" ] ||
   [ ! -s "${STAGE_PLAYBACK_DEVICE_FILE}" ] ||
   [ ! -s "${ASOUND_STAGED}" ] ||
   [ ! -s "${PCP_STAGED}" ]; then
    echo "ERROR: One or more staged installation files are missing."
    exit 1
fi

if [ ! -x "${BUILD_DIR}/usr/local/camilladsp" ] ||
   [ ! -x "${BUILD_DIR}/usr/local/camillagui_backend/camillagui_backend" ] ||
   [ ! -x "${RUST_RUNTIME_BIN}" ]; then
    echo "ERROR: Staged runtime is incomplete."
    exit 1
fi

echo
echo "All downloads, builds and validations completed successfully."
echo "Committing piCoreDSP changes to piCorePlayer..."

###############################################################################
# Transactional commit
###############################################################################

prepare_rollback
COMMIT_STARTED=true

# Persistent CamillaDSP data is changed only now, after all validation passed.
sudo mkdir -p "${DATA_DIR}" "${CONFIG_DIR}" "${COEFF_DIR}"
sudo cp -f "${STAGE_DEFAULT_CONFIG}" "${DEFAULT_CONFIG}"
sudo cp -f "${STAGE_BYPASS_CONFIG}" "${BYPASS_CONFIG}"
sudo cp -f "${STAGE_NULL_CONFIG}" "${NULL_CONFIG}"
sudo cp -f "${STAGE_STATEFILE}" "${STATEFILE}"
sudo cp -f "${STAGE_PLAYBACK_DEVICE_FILE}" "${PLAYBACK_DEVICE_FILE}"

# Set the active config symlink.  On a first install always point to Bypass.
# On reinstall, preserve the user's current selection when the target is a
# valid YAML file inside CONFIG_DIR; fall back to Bypass otherwise.
# Read the existing symlink target BEFORE removing it.
_new_active_target="${BYPASS_CONFIG}"
if $EXISTING_INSTALL; then
    _old_active=$(readlink -f "${ACTIVE_CONFIG_LINK}" 2>/dev/null || true)
    if [ -f "${_old_active}" ] && \
       echo "${_old_active}" | grep -q "^${CONFIG_DIR}/"; then
        _new_active_target="${_old_active}"
    fi
fi
sudo rm -f "${ACTIVE_CONFIG_LINK}"
sudo ln -s "${_new_active_target}" "${ACTIVE_CONFIG_LINK}"

# Install extension before routing live audio to it. Remove a stale dependency
# file from an older Python-based piCoreDSP install, if one remains.
sudo mv -f "${TCZ_TMP}" "${FINAL_TCZ}"
sudo rm -f "${FINAL_DEP}"

if ! grep -qx "${EXTENSION_NAME}.tcz" "${ONBOOT_LIST}"
then
    echo "${EXTENSION_NAME}.tcz" |
        sudo tee -a "${ONBOOT_LIST}" >/dev/null
fi

# Commit ALSA PCM definition.
sudo touch /etc/asound.conf
sudo chmod 664 /etc/asound.conf
sudo chown root:staff /etc/asound.conf
sudo tee /etc/asound.conf < "${ASOUND_STAGED}" >/dev/null

# Route pCP sources LAST, when the extension/config are already in place.
sudo tee "${PCP_CONFIG}" < "${PCP_STAGED}" >/dev/null

sudo chown -R tc:staff "${DATA_DIR}"
sudo chmod -R u+rwX,g+rwX "${DATA_DIR}"

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
echo "          pcm.picoredsp"
echo "                |"
echo "                v"
echo "       hw:Loopback,1,0"
echo "                |"
echo "            snd-aloop"
echo "                |"
echo "                v"
echo "       hw:Loopback,0,0"
echo "                |"
echo "                v"
echo "           CamillaDSP"
echo "                |"
echo "                v"
echo "               DAC"
echo
echo "Active CamillaDSP config after reboot:"
echo "  ${_new_active_target}"
echo
echo "CamillaDSP playback device:"
echo "  ${PLAYBACK_DEVICE}"
echo "Resolved from:"
echo "  ${PLAYBACK_SOURCE}"
echo "Install mode:"
echo "  ${INSTALL_MODE}"
echo
echo "CamillaGUI after reboot:"
echo "  http://pcp.local:5000"
echo
echo "The Bypass config is audible pass-through. Null.yml intentionally discards audio."
echo "Controller runtime: native Rust binary (no Python/pyalsa/pyCamillaDSP dependency)."
echo "First install preserves the physical Squeezelite output."
echo "Reinstall recovers the physical output from CamillaDSP/Bypass/last-known state; it never guesses ALSA card order."
echo
echo "Useful logs:"
echo "  /tmp/picoredsp-startup.log"
echo "  /tmp/camilladsp.log"
echo "  /tmp/camilladsp-supervisor.log"
echo "  /tmp/picoredsp-controller.log"
echo "  /tmp/camillagui-backend.log"
echo

###############################################################################
# Reboot
###############################################################################

pcp reboot
