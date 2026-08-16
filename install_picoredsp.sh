#!/bin/sh -e

###############################################################################
# piCoreCDSP - dual-backend Rust CamillaDSP controller
#
# Architecture:
#   - audio uses one of two transports selected by INSTALL_BACKEND:
#       * aloop: the proven ALSA snd-aloop path;
#       * ioplug: a direct ALSA ioplug PCM -> CamillaDSP stdin path;
#   - the Python/pyalsa/pyCamillaDSP runtime is replaced by one Rust binary;
#   - the Rust controller source lives in the GitHub repository and pre-built
#     aarch64/armv7 binaries are downloaded from GitHub Releases; no Rust
#     toolchain is required on the piCorePlayer device.
#
# The controller follows the Linux --adapt behavior of
# HEnquist/camilladsp-controller, plus piCoreDSP-specific extensions:
#   * re-read the active_config.yml symlink (CamillaGUI) on every adaptation;
#   * adapt the actual initial rate/format/channels before first start;
#   * re-read the complete wave format after CaptureFormatChange.
#
# The controller stays out of the PCM data path: snd-aloop or the ioplug
# plugin carries audio, while this binary only monitors the selected backend
# and drives the documented WebSocket control API.
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

# Expected SHA256 checksums for the pinned CamillaDSP and CamillaGUI archives.
# These are verified after download to detect corruption or supply-chain tampering.
# When upgrading CDSP_VERSION or CAMILLA_GUI_VERSION, update these values.
CDSP_SHA256_AARCH64="d9a17092923ebfe5d20a770c6b6a7eb2268f9700f999bf604b9db09f518aca5a"
CDSP_SHA256_ARMV7="dd1af57129e078383e2a1d5dc28cc13f3f02a78dce9247eb7d9232731b8f7609"
GUI_SHA256_AARCH64="9a5415b44dda58478f18de9fd572edf092f659fd5e45cbe8086ff5648dc089d7"
GUI_SHA256_ARMV7="22b89033ebfe1e4d49afd80c0c745bb6bffec19bc2ac2a60279e565524d467d1"

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
BACKEND_SELECTION_FILE="${DATA_DIR}/backend.conf"
IPC_SOCKET_DIR="/run/picoredsp"
IPC_SOCKET_PATH="${IPC_SOCKET_DIR}/control.sock"

STAGE_CONFIG_DIR="${STAGE_DATA_DIR}/configs"
STAGE_DEFAULT_CONFIG="${STAGE_DATA_DIR}/default_config.yml"
STAGE_BYPASS_CONFIG="${STAGE_CONFIG_DIR}/Bypass.yml"
STAGE_NULL_CONFIG="${STAGE_CONFIG_DIR}/Null.yml"
STAGE_STATEFILE="${STAGE_DATA_DIR}/camilladsp_statefile.yml"
STAGE_ACTIVE_CONFIG_LINK="${STAGE_DATA_DIR}/active_config.yml"
STAGE_PLAYBACK_DEVICE_FILE="${STAGE_DATA_DIR}/playback_device.txt"
STAGE_BACKEND_SELECTION_FILE="${STAGE_DATA_DIR}/backend.conf"

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
IOPLUG_RUNTIME_DIR="${BUILD_DIR}/usr/local/lib/alsa-lib"
IOPLUG_RUNTIME_SO="${IOPLUG_RUNTIME_DIR}/libasound_module_pcm_picoredsp.so"

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
    backup_path "${BACKEND_SELECTION_FILE}" backend.conf
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
        restore_path "${BACKEND_SELECTION_FILE}" backend.conf
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
    echo "Usage: $0 [-k|--keep-downloads] [--backend aloop|ioplug]"
    echo
    echo "  -k, --keep-downloads   Keep downloaded archives in ${CACHE_DIR}"
    echo "      --backend BACKEND  Select aloop (recommended) or ioplug (experimental)"
}

keepDownloads=false
requestedBackend=""

while [ "$#" -gt 0 ]
do
    case "$1" in
        -k|--keep-downloads)
            keepDownloads=true
            ;;
        --backend)
            shift
            if [ "$#" -eq 0 ]; then
                echo "ERROR: --backend requires a value (aloop or ioplug)."
                exit 1
            fi
            requestedBackend="$1"
            ;;
        --backend=*)
            requestedBackend=${1#--backend=}
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
    shift
done

$keepDownloads && echo "Keeping downloads in ${CACHE_DIR}."

backend_label() {
    case "$1" in
        aloop) echo "snd-aloop (recommended / stable)" ;;
        ioplug) echo "direct ioplug (experimental)" ;;
        *) return 1 ;;
    esac
}

currentBackend=""
if [ -f "${BACKEND_SELECTION_FILE}" ]; then
    IFS= read -r currentBackend < "${BACKEND_SELECTION_FILE}" || true
    case "${currentBackend}" in
        aloop|ioplug) ;;
        *) currentBackend="" ;;
    esac
fi

INSTALL_BACKEND="${requestedBackend}"
if [ -n "${INSTALL_BACKEND}" ]; then
    case "${INSTALL_BACKEND}" in
        aloop|ioplug) ;;
        *)
            echo "ERROR: --backend must be 'aloop' or 'ioplug'."
            exit 1
            ;;
    esac
elif [ -t 0 ]; then
    defaultChoice=1
    if [ "${currentBackend}" = "ioplug" ]; then
        defaultChoice=2
    fi

    echo
    echo "Select piCoreDSP backend:"
    echo "  1) snd-aloop (recommended / stable)"
    echo "  2) direct ioplug (experimental)"
    printf "Choice [%s]: " "${defaultChoice}"
    IFS= read -r backendChoice || backendChoice=""
    case "${backendChoice:-${defaultChoice}}" in
        1) INSTALL_BACKEND="aloop" ;;
        2) INSTALL_BACKEND="ioplug" ;;
        *)
            echo "ERROR: Invalid backend selection: ${backendChoice}"
            exit 1
            ;;
    esac
else
    INSTALL_BACKEND="${currentBackend:-aloop}"
    echo "Non-interactive install: using backend ${INSTALL_BACKEND}."
fi

INSTALL_BACKEND_LABEL=$(backend_label "${INSTALL_BACKEND}") || {
    echo "ERROR: Unsupported backend: ${INSTALL_BACKEND}"
    exit 1
}
echo "Selected backend: ${INSTALL_BACKEND_LABEL}"

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

# Even when the TCZ is absent (e.g. deleted manually), old piCoreDSP processes
# may still be running from the previously loaded extension in RAM.  Installing
# over a live runtime risks race conditions and process conflicts.
if [ -e "/usr/local/tce.installed/${EXTENSION_NAME}" ]; then
    echo "ERROR: piCoreDSP is still loaded in RAM. Reboot first."
    echo
    echo "Reboot piCorePlayer before reinstalling."
    exit 1
fi

_running_procs=""
if pgrep -x picoredsp-controller >/dev/null 2>&1; then
    _running_procs="${_running_procs} picoredsp-controller"
fi
if pgrep -x camilladsp >/dev/null 2>&1; then
    _running_procs="${_running_procs} camilladsp"
fi
if pgrep -x camillagui_backend >/dev/null 2>&1; then
    _running_procs="${_running_procs} camillagui_backend"
fi

if [ -n "${_running_procs}" ]; then
    echo "ERROR: Existing piCoreDSP runtime processes are still running:"
    echo " ${_running_procs}"
    echo
    echo "Reboot piCorePlayer before reinstalling."
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
# Test ALSA Loopback support (aloop backend only)
###############################################################################

if [ "${INSTALL_BACKEND}" = "aloop" ]; then
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
else
    echo "Skipping ALSA Loopback probe for ioplug backend."
fi

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
if [ "${INSTALL_BACKEND}" = "aloop" ]; then
    echo "Probing snd-aloop controls with picoredsp-controller..."
    "${RUST_RUNTIME_BIN}" --probe --device hw:Loopback,0
else
    echo "Skipping snd-aloop controller probe for ioplug backend."
fi

###############################################################################
# Download picoredsp ioplug ALSA module
###############################################################################

IOPLUG_RELEASE_URL="https://github.com/${CONTROLLER_REPO}/releases/download/${CONTROLLER_RELEASE_TAG}/libasound_module_pcm_picoredsp-${controller_arch}.so"

echo "Downloading picoredsp ioplug module ${CONTROLLER_RELEASE_TAG} for ${controller_arch}..."

mkdir -p "${IOPLUG_RUNTIME_DIR}"

if $keepDownloads; then
    mkdir -p "${CACHE_DIR}"
    CACHED_IOPLUG="${CACHE_DIR}/libasound_module_pcm_picoredsp-${controller_arch}-${CONTROLLER_RELEASE_TAG}.so"
    if [ "${CONTROLLER_RELEASE_TAG}" = "installer-latest" ]; then
        echo "Rolling tag ${CONTROLLER_RELEASE_TAG}: skipping cache, re-downloading."
        wget -O "${CACHED_IOPLUG}" "${IOPLUG_RELEASE_URL}"
    elif [ ! -f "${CACHED_IOPLUG}" ]; then
        wget -O "${CACHED_IOPLUG}" "${IOPLUG_RELEASE_URL}"
    else
        echo "Using cached ${CACHED_IOPLUG}"
    fi
    cp "${CACHED_IOPLUG}" "${IOPLUG_RUNTIME_SO}"
else
    wget -O "${IOPLUG_RUNTIME_SO}" "${IOPLUG_RELEASE_URL}"
fi

chmod 755 "${IOPLUG_RUNTIME_SO}"

_ioplug_sha256_tmp="/tmp/libasound_module_pcm_picoredsp-${controller_arch}.sha256.$$"
wget -O "${_ioplug_sha256_tmp}" "${IOPLUG_RELEASE_URL}.sha256" \
    || { echo "ERROR: Failed to download SHA256 checksum for libasound_module_pcm_picoredsp-${controller_arch}.so."; exit 1; }
_expected_ioplug_hash=$(awk '{print $1; exit}' "${_ioplug_sha256_tmp}")
_actual_ioplug_hash=$(sha256sum "${IOPLUG_RUNTIME_SO}" | awk '{print $1}')
rm -f "${_ioplug_sha256_tmp}"
if [ "${_expected_ioplug_hash}" != "${_actual_ioplug_hash}" ]; then
    echo "ERROR: SHA256 mismatch for libasound_module_pcm_picoredsp-${controller_arch}.so."
    echo "  Expected: ${_expected_ioplug_hash}"
    echo "  Got:      ${_actual_ioplug_hash}"
    exit 1
fi
echo "picoredsp ioplug module SHA256 verified: ${_actual_ioplug_hash}"

if [ ! -f "${IOPLUG_RUNTIME_SO}" ]; then
    echo "ERROR: picoredsp ioplug module was not downloaded."
    exit 1
fi

if command -v ldd >/dev/null 2>&1; then
    if ldd "${IOPLUG_RUNTIME_SO}" 2>&1 | grep -q 'not found'; then
        echo "ERROR: picoredsp ioplug module has unresolved runtime libraries:"
        ldd "${IOPLUG_RUNTIME_SO}" 2>&1 || true
        exit 1
    fi
fi


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
                ACTIVE_CONFIG_TARGET=$("${RUST_RUNTIME_BIN}" --get-config-path "${STATEFILE}" 2>/dev/null || true)
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
  IMPORTANT: This description is longer than the visible CamillaGUI field.
  Scroll down and read it completely before changing or applying this config.

  Diagnostic-only piCoreDSP configuration.
  Audio is captured from snd-aloop and intentionally discarded.
  Do not select this configuration when testing audible output.

  This YAML file is a persistent baseline configuration. It does not
  necessarily contain the parameters currently used by CamillaDSP.

  picoredsp-controller monitors the active piCoreDSP stream and adapts the
  CamillaDSP runtime configuration in memory to the current sample rate and,
  where applicable, capture format and channel count.

  Values in this YAML or in the CamillaGUI config editor can therefore differ
  from the current live values shown in CamillaGUI's status area. The live
  status values are authoritative for the stream currently playing.

  Runtime-adapted values are not written back to this file.

  Save persistent DSP/config changes in CamillaGUI and run "pcp backup"
  before rebooting piCorePlayer after configuration changes.
EOF

# The statefile contains FINAL runtime paths even though it is staged here.
# On reinstall, preserve any existing volume/mute values so the current
# speaker levels are not silently reset to 0 dB.
#
# Determine the active config that will be used after commit. On first install
# it is Bypass. On reinstall it may remain on a user-selected custom config.
_new_active_target="${BYPASS_CONFIG}"
if $EXISTING_INSTALL; then
    _old_active=$(readlink -f "${ACTIVE_CONFIG_LINK}" 2>/dev/null || true)
    if [ -f "${_old_active}" ] && \
       echo "${_old_active}" | grep -q "^${CONFIG_DIR}/"; then
        _new_active_target="${_old_active}"
    fi
fi

if $EXISTING_INSTALL && [ -f "${STATEFILE}" ]; then
    if ! "${RUST_RUNTIME_BIN}" --make-statefile \
            --config-path "${_new_active_target}" \
            --existing-state "${STATEFILE}" \
            --output "${STAGE_STATEFILE}"; then
        echo "ERROR: Existing CamillaDSP statefile is invalid or cannot be parsed."
        echo "  File: ${STATEFILE}"
        echo "Refusing to silently reset saved volume/mute state to defaults."
        echo "Remove or repair the statefile before reinstalling."
        exit 1
    fi
else
    "${RUST_RUNTIME_BIN}" --make-statefile \
        --config-path "${_new_active_target}" \
        --output "${STAGE_STATEFILE}"
fi

ln -sfn "${STAGE_BYPASS_CONFIG}" "${STAGE_ACTIVE_CONFIG_LINK}"
printf '%s\n' "${PLAYBACK_DEVICE}" > "${STAGE_PLAYBACK_DEVICE_FILE}"
printf '%s\n' "${INSTALL_BACKEND}" > "${STAGE_BACKEND_SELECTION_FILE}"

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
    found_end = 1
    found_old_end = 1
}

/^# BEGIN piCoreDSP$/ {
    newblock = 1
    found_end = 0
    next
}

/^# END piCoreDSP$/ {
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
        print "ERROR: /etc/asound.conf contains a \"# BEGIN piCoreDSP\" marker without a" \
              " matching \"# END piCoreDSP\" marker." > "/dev/stderr"
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

case "${INSTALL_BACKEND}" in
    aloop)
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
        ;;
    ioplug)
        cat >> "${ASOUND_STAGED}" <<EOF

# BEGIN piCoreDSP

pcm.picoredsp {
    type plug

    slave {
        pcm {
            type picoredsp
            socket_path "${IPC_SOCKET_PATH}"
        }
        channels 2
    }

    hint {
        show on
        description "piCoreDSP direct ioplug (experimental)"
    }
}

# END piCoreDSP
EOF
        ;;
esac

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

# Also validate the ioplug adaptation path when installing with the ioplug
# backend.  This ensures capture.type: Stdin is injected correctly and that
# the playback device is preserved, catching bugs that the aloop-only check
# above would miss.
if [ "${INSTALL_BACKEND}" = "ioplug" ]; then
    RUST_IOPLUG_ADAPTED_TEST="/tmp/picoredsp-adapted-ioplug.$$.yml"
    "${RUST_RUNTIME_BIN}" \
        --adapt-check \
        --backend ioplug \
        --adapt "${STAGE_BYPASS_CONFIG}" \
        --rate 48000 \
        --format S32_LE \
        --channels 2 \
        > "${RUST_IOPLUG_ADAPTED_TEST}"

    if ! grep -Eq '^[[:space:]]*type:[[:space:]]*Stdin[[:space:]]*$' "${RUST_IOPLUG_ADAPTED_TEST}"; then
        echo "ERROR: ioplug adaptation did not inject capture.type: Stdin."
        exit 1
    fi

    if [ "$("${RUST_RUNTIME_BIN}" --get-playback-device "${RUST_IOPLUG_ADAPTED_TEST}" 2>/dev/null || true)" != "${PLAYBACK_DEVICE}" ]; then
        echo "ERROR: ioplug adaptation did not preserve the selected playback device."
        exit 1
    fi

    rm -f "${RUST_IOPLUG_ADAPTED_TEST}"
fi

echo "picoredsp-controller ${CONTROLLER_RELEASE_TAG} download and ALSA probe OK."

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
    TEST_STDERR_LOG="${TEST_LOG}.stderr"
    TEST_STATEFILE="/tmp/picoredsp-camilladsp-ws-test.$$.state.yml"
    TEST_PID=""

    cleanup_ws_test() {
        if [ -n "${TEST_PID}" ]; then
            kill "${TEST_PID}" >/dev/null 2>&1 || true
            wait "${TEST_PID}" >/dev/null 2>&1 || true
        fi
        rm -f "${TEST_LOG}" "${TEST_STDERR_LOG}" "${TEST_STATEFILE}" >/dev/null 2>&1 || true
    }

    trap cleanup_ws_test EXIT HUP INT TERM

    if ! cp "${STAGE_STATEFILE}" "${TEST_STATEFILE}"; then
        echo "ERROR: Failed to stage temporary CamillaDSP statefile for WebSocket smoke test."
        exit 1
    fi

    "${BUILD_DIR}/usr/local/camilladsp" \
        --wait \
        --no_config \
        --statefile "${TEST_STATEFILE}" \
        --port "${TEST_PORT}" \
        --address 127.0.0.1 \
        --logfile "${TEST_LOG}" \
        >"${TEST_STDERR_LOG}" 2>&1 &
    TEST_PID=$!

    i=0
    while [ "${i}" -lt 20 ]
    do
        if ! kill -0 "${TEST_PID}" 2>/dev/null; then
            echo "ERROR: Temporary CamillaDSP exited before WebSocket became available."
            cat "${TEST_STDERR_LOG}" 2>/dev/null || true
            cat "${TEST_LOG}" 2>/dev/null || true
            exit 1
        fi

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
        cat "${TEST_STDERR_LOG}" 2>/dev/null || true
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

download_and_extract_tar_gz \
    "camillagui-${CAMILLA_GUI_VERSION}-${architecture}.tar.gz" \
    "https://github.com/HEnquist/camillagui-backend/releases/download/${CAMILLA_GUI_VERSION}/bundle_linux_${architecture}.tar.gz" \
    "${GUI_SHA256}"

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

# CamillaGUI validates every config it loads against supported_capture_types.
# For the ioplug backend the controller injects capture.type: Stdin at
# runtime, so Stdin must be listed here; otherwise CamillaGUI immediately
# rejects the adapted config with "'Stdin' is not one of ['Alsa']".
if [ "${INSTALL_BACKEND}" = "ioplug" ]; then
    _CAMILLAGUI_CAPTURE_TYPES='  - "Alsa"
  - "Stdin"'
else
    _CAMILLAGUI_CAPTURE_TYPES='  - "Alsa"'
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
log_file: "/tmp/camilladsp_rCURRENT.log"

# on_set_active_config intentionally does NOT use the {} placeholder.
# CamillaGUI calls this via os.system(), which evaluates shell metacharacters
# before invoking the command.  Using {} would allow a config file named with
# shell metacharacters (e.g. "\$(cmd)") to execute arbitrary commands as tc.
# The wrapper queries the config path from CamillaDSP via WebSocket instead,
# so no user-controlled string ever reaches the shell.
on_set_active_config: "/usr/local/bin/picoredsp-sync-config"
on_get_active_config: "readlink -f ${ACTIVE_CONFIG_LINK}"

supported_capture_types:
${_CAMILLAGUI_CAPTURE_TYPES}

supported_playback_types:
  - "Alsa"
EOF

###############################################################################
# piCoreDSP set-active-config wrapper
# Reads the active config path via WebSocket (GetConfigFilePath) when
# CamillaDSP is online (synchronously updated, no statefile race).
# Falls back to reading the statefile when CamillaDSP is offline
# (CamillaGUI writes the path directly to the statefile in that case).
###############################################################################

mkdir -p "${BUILD_DIR}/usr/local/bin"

cat > "${BUILD_DIR}/usr/local/bin/picoredsp-apply-backend" <<EOF
#!/bin/sh
BACKEND_FILE="${BACKEND_SELECTION_FILE}"
ASOUND_TARGET="/etc/asound.conf"
IPC_SOCKET_PATH="${IPC_SOCKET_PATH}"

backend="\${1:-}"
if [ -z "\${backend}" ] && [ -f "\${BACKEND_FILE}" ]; then
    IFS= read -r backend < "\${BACKEND_FILE}" || backend=""
fi

case "\${backend}" in
    aloop|ioplug) ;;
    *)
        echo "picoredsp-apply-backend: backend must be aloop or ioplug" >&2
        exit 1
        ;;
esac

tmp="/tmp/asound.conf.picoredsp-apply.\$\$"
trap 'rm -f "\${tmp}"' EXIT HUP INT TERM

asound_source=/dev/null
if [ -f "\${ASOUND_TARGET}" ]; then
    asound_source="\${ASOUND_TARGET}"
fi

awk '
BEGIN {
    newblock = 0
    oldblock = 0
    found_end = 1
    found_old_end = 1
}

/^# BEGIN piCoreDSP$/ {
    newblock = 1
    found_end = 0
    next
}

/^# END piCoreDSP$/ {
    newblock = 0
    found_end = 1
    next
}

/^# For more info about this configuration see: .*alsa_cdsp/ {
    oldblock = 1
    found_old_end = 0
    next
}

oldblock && /^# pcm\\.camilladsp$/ {
    oldblock = 0
    found_old_end = 1
    next
}

!newblock && !oldblock {
    print
}

END {
    if (!found_end) {
        print "ERROR: /etc/asound.conf contains a \\"# BEGIN piCoreDSP\\" marker without a matching \\"# END piCoreDSP\\" marker." > "/dev/stderr"
        exit 1
    }
    if (!found_old_end) {
        print "ERROR: /etc/asound.conf contains an old alsa_cdsp block that is missing its closing \\"# pcm.camilladsp\\" marker." > "/dev/stderr"
        exit 1
    }
}
' "\${asound_source}" > "\${tmp}" || exit 1

case "\${backend}" in
    aloop)
        cat >> "\${tmp}" <<'BLOCK'

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
BLOCK
        ;;
    ioplug)
        cat >> "\${tmp}" <<BLOCK

# BEGIN piCoreDSP

pcm.picoredsp {
    type plug

    slave {
        pcm {
            type picoredsp
            socket_path "${IPC_SOCKET_PATH}"
        }
        channels 2
    }

    hint {
        show on
        description "piCoreDSP direct ioplug (experimental)"
    }
}

# END piCoreDSP
BLOCK
        ;;
esac

sudo touch "\${ASOUND_TARGET}"
sudo chmod 664 "\${ASOUND_TARGET}"
sudo chown root:staff "\${ASOUND_TARGET}"
sudo tee "\${ASOUND_TARGET}" < "\${tmp}" >/dev/null
EOF

chmod 755 "${BUILD_DIR}/usr/local/bin/picoredsp-apply-backend"

cat > "${BUILD_DIR}/usr/local/bin/picoredsp-sync-config" <<EOF
#!/bin/sh
# Called by CamillaGUI on_set_active_config.  Reads the active config path
# via WebSocket when CamillaDSP is online (GetConfigFilePath is synchronously
# updated, avoiding the ~1 s statefile write delay).  Falls back to the
# statefile when CamillaDSP is offline (CamillaGUI writes the path directly
# to the statefile in that case).  The CamillaGUI-supplied argument is
# intentionally discarded to prevent shell injection.
ACTIVE_CONFIG_LINK="${ACTIVE_CONFIG_LINK}"
CONTROLLER="/usr/local/bin/picoredsp-controller"
CONFIG_DIR="${CONFIG_DIR}"
STATEFILE="${STATEFILE}"

# Try WebSocket first (CamillaDSP online: synchronously updated in-memory path
# avoids the ~1 s statefile write delay).  Retry briefly in case the DSP is
# mid-restart.  Fall back to reading the statefile when CamillaDSP is offline
# (e.g. CamillaGUI wrote the path directly to the statefile while DSP was down).
config_path=""
_ws_attempt=0
while [ "\${_ws_attempt}" -lt 3 ]; do
    config_path=\$("\${CONTROLLER}" --ws-get-config-path --host 127.0.0.1 --port 1234 2>/dev/null) \
        && break
    _ws_attempt=\$((_ws_attempt + 1))
    [ "\${_ws_attempt}" -lt 3 ] && sleep 1
done

if [ -z "\${config_path}" ]; then
    config_path=\$("\${CONTROLLER}" --get-config-path "\${STATEFILE}") || {
        echo "picoredsp-sync-config: failed to read config path from CamillaDSP and statefile" >&2
        exit 1
    }
fi

canonical=\$(readlink -f "\${config_path}") || {
    echo "picoredsp-sync-config: failed to resolve config path: \${config_path}" >&2
    exit 1
}

case "\${canonical}" in
    "\${CONFIG_DIR}/"*) ;;
    *)
        echo "picoredsp-sync-config: config path outside CONFIG_DIR: \${canonical}" >&2
        exit 1
        ;;
esac

[ -f "\${canonical}" ] || {
    echo "picoredsp-sync-config: config file does not exist: \${canonical}" >&2
    exit 1
}

ln -sfn "\${canonical}" "\${ACTIVE_CONFIG_LINK}"
EOF

chmod 755 "${BUILD_DIR}/usr/local/bin/picoredsp-sync-config"

# Helper script shared by all three supervisor loops.
cat > "${BUILD_DIR}/usr/local/bin/picoredsp-trim-log" <<'EOF'
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
chmod 755 "${BUILD_DIR}/usr/local/bin/picoredsp-trim-log"

cat > "${BUILD_DIR}/usr/local/bin/picoredsp-switch-backend" <<EOF
#!/bin/sh
BACKEND_FILE="${BACKEND_SELECTION_FILE}"
DEFAULT_BACKEND="aloop"

backend_label() {
    case "\$1" in
        aloop) echo "snd-aloop (recommended / stable)" ;;
        ioplug) echo "direct ioplug (experimental)" ;;
        *) return 1 ;;
    esac
}

preflight_aloop_backend() {
    if ! sudo modprobe snd-aloop; then
        echo "ERROR: snd-aloop could not be loaded; backend selection unchanged." >&2
        echo "This piCorePlayer kernel/image needs ALSA Loopback support." >&2
        return 1
    fi

    for _retry in 1 2 3 4 5 6 7 8 9 10
    do
        if grep -q "Loopback" /proc/asound/cards 2>/dev/null; then
            return 0
        fi

        sleep 1
    done

    echo "ERROR: snd-aloop loaded, but the Loopback card did not appear; backend selection unchanged." >&2
    return 1
}

target="\${1:-}"
rebootNow=false

if [ "\${target}" = "--reboot" ]; then
    rebootNow=true
    target="\${2:-}"
fi

if [ -z "\${target}" ]; then
    current="\${DEFAULT_BACKEND}"
    if [ -f "\${BACKEND_FILE}" ]; then
        IFS= read -r current < "\${BACKEND_FILE}" || current="\${DEFAULT_BACKEND}"
    fi
    case "\${current}" in
        aloop|ioplug) ;;
        *) current="\${DEFAULT_BACKEND}" ;;
    esac
    defaultChoice=1
    if [ "\${current}" = "ioplug" ]; then
        defaultChoice=2
    fi
    echo "Select piCoreDSP backend:"
    echo "  1) snd-aloop (recommended / stable)"
    echo "  2) direct ioplug (experimental)"
    printf "Choice [%s]: " "\${defaultChoice}"
    IFS= read -r backendChoice || backendChoice=""
    case "\${backendChoice:-\${defaultChoice}}" in
        1) target="aloop" ;;
        2) target="ioplug" ;;
        *)
            echo "ERROR: Invalid backend selection: \${backendChoice}" >&2
            exit 1
            ;;
    esac
fi

case "\${target}" in
    aloop|ioplug) ;;
    *)
        echo "Usage: picoredsp-switch-backend [--reboot] [aloop|ioplug]" >&2
        exit 1
        ;;
esac

if [ "\${target}" = "aloop" ]; then
    preflight_aloop_backend || exit 1
fi

sudo mkdir -p "\$(dirname "\${BACKEND_FILE}")"
printf '%s\n' "\${target}" | sudo tee "\${BACKEND_FILE}" >/dev/null
sudo chown tc:staff "\${BACKEND_FILE}" 2>/dev/null || true
sudo chmod 664 "\${BACKEND_FILE}" 2>/dev/null || true

pcp backup

echo "Backend saved: \$(backend_label "\${target}")"
echo "Switching backends requires a reboot before the new ALSA route and controller mode take effect."
echo "Do not start new playback until after reboot."

if [ "\${rebootNow}" = true ]; then
    pcp reboot
else
    echo "Run: pcp reboot"
fi
EOF

chmod 755 "${BUILD_DIR}/usr/local/bin/picoredsp-switch-backend"

###############################################################################
# Boot script
###############################################################################

mkdir -p "${BUILD_DIR}/usr/local/tce.installed"

cat > "${BUILD_DIR}/usr/local/tce.installed/${EXTENSION_NAME}" <<'EOF'
#!/bin/sh

CONTROLLER="/usr/local/bin/picoredsp-controller"
ACTIVE_CONFIG="/mnt/mmcblk0p2/tce/camilladsp/active_config.yml"
STATEFILE="/mnt/mmcblk0p2/tce/camilladsp/camilladsp_statefile.yml"
BACKEND_FILE="/mnt/mmcblk0p2/tce/camilladsp/backend.conf"
IPC_SOCKET_DIR="/run/picoredsp"
IPC_SOCKET_PATH="${IPC_SOCKET_DIR}/control.sock"

STARTUP_LOG="/tmp/picoredsp-startup.log"

echo "$(date): piCoreDSP startup" >> "${STARTUP_LOG}"
echo "$(date): active config: $(readlink -f "${ACTIVE_CONFIG}" 2>/dev/null)" >> "${STARTUP_LOG}"
BACKEND="aloop"
if [ -f "${BACKEND_FILE}" ]; then
    IFS= read -r BACKEND < "${BACKEND_FILE}" || BACKEND="aloop"
fi
case "${BACKEND}" in
    aloop|ioplug) ;;
    *)
        echo "$(date): invalid backend '${BACKEND}', falling back to aloop" >> "${STARTUP_LOG}"
        BACKEND="aloop"
        ;;
esac
echo "$(date): backend: ${BACKEND}" >> "${STARTUP_LOG}"

if ! /usr/local/bin/picoredsp-apply-backend "${BACKEND}" >> "${STARTUP_LOG}" 2>&1; then
    echo "$(date): failed to apply ALSA config for backend ${BACKEND}" >> "${STARTUP_LOG}"
    exit 1
fi

###############################################################################
# Backend-specific runtime
###############################################################################

case "${BACKEND}" in
    aloop)
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
            /usr/local/bin/picoredsp-trim-log "${_log}"

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
                >> /tmp/picoredsp-startup.log

            sleep 2
        done
        ' &

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
            echo "$(date): CamillaDSP websocket did not become ready" >> "${STARTUP_LOG}"
        fi
        ;;
    ioplug)
        mkdir -p "${IPC_SOCKET_DIR}"
        chown tc:staff "${IPC_SOCKET_DIR}" 2>/dev/null || true
        chmod 775 "${IPC_SOCKET_DIR}" 2>/dev/null || true
        rm -f "${IPC_SOCKET_PATH}" 2>/dev/null || true
        ;;
esac

###############################################################################
# Periodic log trimmer (bounds long-running logs even without restarts)
###############################################################################

sudo -u tc sh -c '
exec >> /tmp/picoredsp-logtrim.log 2>&1
while :
do
    /usr/local/bin/picoredsp-trim-log /tmp/picoredsp-controller.log
    /usr/local/bin/picoredsp-trim-log /tmp/camillagui-backend.log
    sleep 60
done
' &

###############################################################################
# Controller supervisor (loads config when playback becomes active)
###############################################################################

sudo -u tc sh -c '
exec >> /tmp/picoredsp-controller.log 2>&1
_log=/tmp/picoredsp-controller.log
while :
do
    /usr/local/bin/picoredsp-trim-log "${_log}"

    BACKEND="aloop"
    if [ -f /mnt/mmcblk0p2/tce/camilladsp/backend.conf ]; then
        IFS= read -r BACKEND < /mnt/mmcblk0p2/tce/camilladsp/backend.conf || BACKEND="aloop"
    fi

    case "${BACKEND}" in
        ioplug)
            # `--adapt` remains the persistent baseline input. The controller
            # writes the per-stream runtime YAML to `/run/picoredsp/`.
            /usr/local/bin/picoredsp-controller \
                --backend ioplug \
                --socket-path /run/picoredsp/control.sock \
                --camilladsp /usr/local/camilladsp \
                --adapt /mnt/mmcblk0p2/tce/camilladsp/active_config.yml \
                --host 127.0.0.1 \
                --port 1234 \
                --cdsp-statefile /mnt/mmcblk0p2/tce/camilladsp/camilladsp_statefile.yml \
                --log-level INFO
            ;;
        *)
            /usr/local/bin/picoredsp-controller \
                --host 127.0.0.1 \
                --port 1234 \
                --device hw:Loopback,0 \
                --adapt /mnt/mmcblk0p2/tce/camilladsp/active_config.yml \
                --log-level INFO
            ;;
    esac

    rc=$?

    echo "$(date): Rust piCoreDSP controller exited with ${rc}; restarting" \
        >> /tmp/picoredsp-startup.log

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
    /usr/local/bin/picoredsp-trim-log "${_log}"

    /usr/local/camillagui_backend/camillagui_backend

    rc=$?

    echo "$(date): CamillaGUI exited with ${rc}; restarting" \
        >> /tmp/picoredsp-startup.log

    sleep 2
done
' &

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
# already present in the piCorePlayer ALSA stack. No .tcz.dep is installed.

###############################################################################
# Final pre-commit validation
###############################################################################

if [ ! -s "${STAGE_DEFAULT_CONFIG}" ] ||
   [ ! -s "${STAGE_BYPASS_CONFIG}" ] ||
   [ ! -s "${STAGE_NULL_CONFIG}" ] ||
   [ ! -s "${STAGE_STATEFILE}" ] ||
   [ ! -s "${STAGE_PLAYBACK_DEVICE_FILE}" ] ||
   [ ! -s "${STAGE_BACKEND_SELECTION_FILE}" ] ||
   [ ! -s "${ASOUND_STAGED}" ] ||
   [ ! -s "${PCP_STAGED}" ]; then
    echo "ERROR: One or more staged installation files are missing."
    exit 1
fi

if [ ! -x "${BUILD_DIR}/usr/local/camilladsp" ] ||
   [ ! -x "${BUILD_DIR}/usr/local/camillagui_backend/camillagui_backend" ] ||
   [ ! -x "${RUST_RUNTIME_BIN}" ] ||
   [ ! -f "${IOPLUG_RUNTIME_SO}" ]; then
    echo "ERROR: Staged runtime is incomplete."
    exit 1
fi

# Validate the already selected post-commit active target before COMMIT_STARTED
# so a broken preserved config aborts cleanly without requiring a rollback.

if [ "${_new_active_target}" != "${BYPASS_CONFIG}" ]; then
    echo "Validating preserved active config: ${_new_active_target}"
    _check_output=$("${BUILD_DIR}/usr/local/camilladsp" --check "${_new_active_target}" 2>&1) || {
        echo "ERROR: The preserved active CamillaDSP config failed validation:"
        echo "  ${_new_active_target}"
        echo "${_check_output}"
        echo
        echo "Repair or remove the config, then reinstall."
        echo "Alternatively, point the active_config.yml symlink to Bypass.yml first."
        exit 1
    }
    echo "Preserved active config is valid."
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
sudo cp -f "${STAGE_BACKEND_SELECTION_FILE}" "${BACKEND_SELECTION_FILE}"

# Set the active config symlink using the target already determined and
# validated in the pre-commit check above.
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

# Apply ownership and permissions only to the directories and files this
# installer created or modified — not recursively over the entire DATA_DIR.
# This preserves existing metadata on user-created configs and coeff files,
# making rollback fully accurate even if pcp backup later fails.
for _d in "${DATA_DIR}" "${CONFIG_DIR}" "${COEFF_DIR}"; do
    sudo chown tc:staff "${_d}" 2>/dev/null || true
    sudo chmod u+rwx,g+rwx "${_d}" 2>/dev/null || true
done
for _f in "${DEFAULT_CONFIG}" "${BYPASS_CONFIG}" "${NULL_CONFIG}" \
           "${STATEFILE}" "${PLAYBACK_DEVICE_FILE}" "${BACKEND_SELECTION_FILE}"; do
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
echo "          pcm.picoredsp"
echo "                |"
echo "                v"
case "${INSTALL_BACKEND}" in
    aloop)
        echo "       hw:Loopback,1,0"
        echo "                |"
        echo "            snd-aloop"
        echo "                |"
        echo "                v"
        echo "       hw:Loopback,0,0"
        ;;
    ioplug)
        echo "  libasound_module_pcm_picoredsp.so"
        echo "                |"
        echo "                v"
        echo "       AF_UNIX + stdin pipe"
        ;;
esac
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
echo "Backend:"
echo "  ${INSTALL_BACKEND_LABEL}"
echo
echo "CamillaGUI after reboot:"
echo "  http://pcp.local:5000"
echo
echo "The Bypass config is audible pass-through. Null.yml intentionally discards audio."
echo "Controller runtime: native Rust binary (no Python/pyalsa/pyCamillaDSP dependency)."
echo "Use /usr/local/bin/picoredsp-switch-backend [aloop|ioplug] to switch later."
echo "Backend changes are applied on the next explicit reboot."
echo "First install preserves the physical Squeezelite output."
echo "Reinstall recovers the physical output from CamillaDSP/Bypass/last-known state; it never guesses ALSA card order."
echo
echo "Useful logs:"
echo "  /tmp/picoredsp-startup.log"
echo "  /tmp/camilladsp_rCURRENT.log"
echo "  /tmp/camilladsp-supervisor.log"
echo "  /tmp/picoredsp-controller.log"
echo "  /tmp/camillagui-backend.log"
echo

###############################################################################
# Reboot
###############################################################################

pcp reboot
