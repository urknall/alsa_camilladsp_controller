#!/bin/sh -e

###############################################################################
# piCoreCDSP - snd-aloop + Rust CamillaDSP controller
#
# This is phase 2 of the piCoreDSP architecture:
#   - audio stays on the proven ALSA snd-aloop path;
#   - the Python/pyalsa/pyCamillaDSP runtime is replaced by one Rust binary;
#   - the Rust controller intentionally follows the Linux --adapt behavior of
#     HEnquist/camilladsp-controller at e9fde2057d5869e6805a965e9c091bbb9a9e9980,
#     plus the piCoreDSP fixes proven by the Python installer:
#       * re-read CamillaGUI's active_config.yml symlink on every adaptation;
#       * adapt the actual initial rate/format/channels before first start;
#       * re-read the complete wave format after CaptureFormatChange.
#
# The custom controller stays out of the PCM data path: ALSA/snd-aloop carries
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

RUST_CONTROLLER_VERSION="0.1.0"
PYTHON_REFERENCE_CONTROLLER="e9fde2057d5869e6805a965e9c091bbb9a9e9980"

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

RUST_PROJECT="/tmp/picoredsp-controller-rust.$$"
RUST_CARGO_HOME_TMP="/tmp/picoredsp-cargo.$$"
RUST_RUNTIME_BIN="${BUILD_DIR}/usr/local/bin/picoredsp-controller"
RUST_SOURCE_DIR="${BUILD_DIR}/usr/local/share/picoredsp-controller"

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
    rm -rf "${RUST_PROJECT}" 2>/dev/null || true
    rm -rf "${RUST_CARGO_HOME_TMP}" 2>/dev/null || true
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

read_cdsp_playback_device() {
    config="$1"
    [ -f "${config}" ] || return 1

    awk '
    function indentation(s) {
        match(s, /^[ \t]*/)
        return RLENGTH
    }
    {
        raw = $0
        indent = indentation(raw)
        line = raw
        sub(/^[ \t]*/, "", line)

        if (line == "" || line ~ /^#/)
            next

        if (!in_devices) {
            if (line ~ /^devices:[ \t]*$/) {
                in_devices = 1
                devices_indent = indent
            }
            next
        }

        if (indent <= devices_indent) {
            in_devices = 0
            in_playback = 0
            next
        }

        if (!in_playback) {
            if (line ~ /^playback:[ \t]*$/) {
                in_playback = 1
                playback_indent = indent
            }
            next
        }

        if (indent <= playback_indent) {
            in_playback = 0
            next
        }

        if (line ~ /^device:[ \t]*/) {
            sub(/^device:[ \t]*/, "", line)
            sub(/[ \t]+$/, "", line)
            sub(/^"/, "", line)
            sub(/"$/, "", line)
            sub(/^\047/, "", line)
            sub(/\047$/, "", line)
            print line
            exit
        }
    }
    ' "${config}"
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
                candidate=$(read_cdsp_playback_device "${ACTIVE_CONFIG_TARGET}" 2>/dev/null || true)
                if is_usable_playback_device "${candidate}"; then
                    PLAYBACK_DEVICE="${candidate}"
                    PLAYBACK_SOURCE="active CamillaDSP config (${ACTIVE_CONFIG_TARGET})"
                fi
            fi

            if [ -z "${PLAYBACK_DEVICE}" ]; then
                candidate=$(read_cdsp_playback_device "${BYPASS_CONFIG}" 2>/dev/null || true)
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
cat > "${STAGE_DEFAULT_CONFIG}" <<EOF
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
    device: "${PLAYBACK_DEVICE}"

filters: {}
mixers: {}
pipeline: []
processors: {}
EOF

cp "${STAGE_DEFAULT_CONFIG}" "${STAGE_BYPASS_CONFIG}"

cat >> "${STAGE_BYPASS_CONFIG}" <<EOF

title: 'Bypass'
description: |
  Default piCoreDSP pass-through configuration.
  Audio is captured from snd-aloop and played to the piCorePlayer output
  that was selected before installation: ${PLAYBACK_DEVICE}

  Add filters/mixers in CamillaGUI or duplicate this config for DSP work.
EOF

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
cat > "${STAGE_STATEFILE}" <<EOF
config_path: ${BYPASS_CONFIG}

mute:
- false
- false
- false
- false
- false

volume:
- 0.0
- 0.0
- 0.0
- 0.0
- 0.0
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

###############################################################################
# Rust controller build environment
###############################################################################

# Rust, the C linker and ALSA headers are BUILD-TIME dependencies only. They
# are not placed in piCoreCDSP.tcz and are not needed after reboot.
install_temporarily_if_missing rust
install_temporarily_if_missing compiletc
install_temporarily_if_missing libasound-dev

if ! command -v pkg-config >/dev/null 2>&1; then
    install_temporarily_if_missing pkg-config
fi

if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: Rust toolchain is not available after loading rust.tcz."
    echo "Both rustc and cargo are required to build the controller."
    exit 1
fi

if ! command -v cc >/dev/null 2>&1; then
    echo "ERROR: C linker/compiler is not available after loading compiletc."
    exit 1
fi

if ! command -v pkg-config >/dev/null 2>&1 || ! pkg-config --exists alsa; then
    echo "ERROR: ALSA development metadata is unavailable."
    echo "libasound-dev and pkg-config are required to build the Rust controller."
    exit 1
fi

echo "Rust compiler: $(rustc --version)"
echo "Cargo:         $(cargo --version)"

RUST_MINOR=$(rustc --version | awk '{split($2, v, "."); print v[2]}')
if [ -z "${RUST_MINOR}" ] || [ "${RUST_MINOR}" -lt 71 ]; then
    echo "ERROR: Rust 1.71 or newer is required by the pinned controller dependencies."
    exit 1
fi

rm -rf "${RUST_PROJECT}"
mkdir -p "${RUST_PROJECT}/src"

if $keepDownloads; then
    mkdir -p "${CACHE_DIR}/cargo"
    export CARGO_HOME="${CACHE_DIR}/cargo"
else
    export CARGO_HOME="${RUST_CARGO_HOME_TMP}"
    mkdir -p "${CARGO_HOME}"
fi

cat > "${RUST_PROJECT}/Cargo.toml" <<'PICORE_CARGO_TOML'
[package]
name = "picoredsp-controller"
version = "0.1.0"
edition = "2021"
rust-version = "1.71"
description = "Rust controller for piCoreDSP snd-aloop -> CamillaDSP integration"
license = "MIT"

[dependencies]
alsa = "=0.11.0"
serde_json = "=1.0.145"
serde_yaml_ng = "=0.10.0"
tungstenite = { version = "=0.29.0", default-features = false, features = ["handshake"] }

[profile.release]
strip = true
lto = true
codegen-units = 1
panic = "abort"
PICORE_CARGO_TOML

cat > "${RUST_PROJECT}/src/main.rs" <<'PICORE_RUST_SOURCE'
use alsa::ctl::ElemIface;
use alsa::hctl::HCtl;
use serde_json::Value as JsonValue;
use serde_yaml_ng::{Mapping, Value as YamlValue};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tungstenite::client::connect;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LOOPBACK_ACTIVE: &str = "PCM Slave Active";
const LOOPBACK_CHANNELS: &str = "PCM Slave Channels";
const LOOPBACK_FORMAT: &str = "PCM Slave Format";
const LOOPBACK_RATE: &str = "PCM Slave Rate";
const GADGET_CAPTURE_RATE: &str = "Capture Rate";

// The Python listener debounces ALSA HCTL changes for 50 ms before reading
// the controls. Keep the same behavior here.
const ALSA_DEBOUNCE_MS: u64 = 50;
// The Python controller checks its event queue / CamillaDSP state every 200 ms.
const CONTROL_LOOP_MS: u32 = 200;

type AppResult<T> = Result<T, Box<dyn Error>>;
type WsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

fn app_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(std::io::Error::new(std::io::ErrorKind::Other, message.into()))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LogLevel {
    Error = 0,
    Warning = 1,
    Info = 2,
    Debug = 3,
}

impl LogLevel {
    fn parse(value: &str) -> AppResult<Self> {
        match value.to_ascii_uppercase().as_str() {
            "CRITICAL" | "ERROR" => Ok(Self::Error),
            "WARNING" | "WARN" => Ok(Self::Warning),
            "INFO" => Ok(Self::Info),
            "DEBUG" => Ok(Self::Debug),
            other => Err(app_error(format!("invalid log level: {other}"))),
        }
    }
}

fn log(level: LogLevel, configured: LogLevel, message: impl AsRef<str>) {
    if level <= configured {
        let name = match level {
            LogLevel::Error => "ERROR",
            LogLevel::Warning => "WARNING",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
        };
        eprintln!("{name} - {}", message.as_ref());
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WaveFormat {
    sample_rate: Option<u32>,
    sample_format: Option<String>,
    channels: Option<u32>,
}

impl fmt::Display for WaveFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rate={:?}, format={:?}, channels={:?}",
            self.sample_rate, self.sample_format, self.channels
        )
    }
}

impl WaveFormat {
    fn with_fallback(&self, fallback: &WaveFormat) -> Self {
        Self {
            sample_rate: self.sample_rate.or(fallback.sample_rate),
            sample_format: self
                .sample_format
                .clone()
                .or_else(|| fallback.sample_format.clone()),
            channels: self.channels.or(fallback.channels),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeviceSnapshot {
    active: bool,
    wave: WaveFormat,
}

struct AlsaLoopbackListener {
    hctl: HCtl,
    device: u32,
    subdevice: u32,
    log_level: LogLevel,
}

impl AlsaLoopbackListener {
    fn new(device_name: &str, log_level: LogLevel) -> AppResult<Self> {
        // Keep the same parsing rules as the Python controller:
        //   hw:Loopback,0,0 -> card=hw:Loopback, device=0, subdevice=0
        //   hw:Loopback,0   -> card=hw:Loopback, device=0, subdevice=0
        let parts: Vec<&str> = device_name.split(',').collect();
        let card = parts
            .first()
            .copied()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| app_error("empty ALSA control device"))?;
        let device = if parts.len() >= 2 {
            parts[1]
                .parse::<u32>()
                .map_err(|_| app_error(format!("invalid ALSA device number in {device_name}")))?
        } else {
            0
        };
        let subdevice = if parts.len() >= 3 {
            parts[2]
                .parse::<u32>()
                .map_err(|_| app_error(format!("invalid ALSA subdevice number in {device_name}")))?
        } else {
            0
        };

        // Non-blocking HCTL matches alsa-python's HControl(... NONBLOCK).
        let hctl = HCtl::new(card, true)?;
        hctl.load()?;

        let listener = Self {
            hctl,
            device,
            subdevice,
            log_level,
        };

        // Fail early if this is not the snd-aloop control device expected by
        // piCoreDSP. This is more useful than silently running with no controls.
        let snapshot = listener.read_snapshot()?;
        log(
            LogLevel::Debug,
            log_level,
            format!("Initial ALSA snapshot: active={}, {}", snapshot.active, snapshot.wave),
        );
        Ok(listener)
    }

    fn wait_for_event(&self, timeout_ms: u32) -> AppResult<bool> {
        Ok(self.hctl.wait(Some(timeout_ms))?)
    }

    fn handle_events(&self) -> AppResult<()> {
        self.hctl.handle_events()?;
        Ok(())
    }

    fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
        let mut loopback_active: Option<bool> = None;
        let mut channels: Option<u32> = None;
        let mut raw_format: Option<i32> = None;
        let mut loopback_rate: Option<u32> = None;
        let mut gadget_rate: Option<u32> = None;

        for elem in self.hctl.elem_iter() {
            let id = elem.get_id()?;
            if id.get_interface() != ElemIface::PCM
                || id.get_device() != self.device
                || id.get_subdevice() != self.subdevice
            {
                continue;
            }

            let name = id.get_name()?.to_owned();
            match name.as_str() {
                LOOPBACK_ACTIVE => {
                    let value = elem.read()?;
                    loopback_active = value.get_boolean(0);
                }
                LOOPBACK_CHANNELS => {
                    let value = elem.read()?;
                    channels = value.get_integer(0).and_then(nonnegative_u32);
                }
                LOOPBACK_FORMAT => {
                    let value = elem.read()?;
                    raw_format = value.get_integer(0);
                }
                LOOPBACK_RATE => {
                    let value = elem.read()?;
                    loopback_rate = value.get_integer(0).and_then(nonnegative_u32);
                }
                GADGET_CAPTURE_RATE => {
                    let value = elem.read()?;
                    gadget_rate = value.get_integer(0).and_then(nonnegative_u32);
                }
                _ => {}
            }
        }

        // Match the Python controller's USB-gadget behavior: Capture Rate,
        // when present, is authoritative and format/channels are unknown.
        if let Some(rate) = gadget_rate {
            return Ok(DeviceSnapshot {
                active: rate > 0,
                wave: WaveFormat {
                    sample_rate: Some(rate),
                    sample_format: None,
                    channels: None,
                },
            });
        }

        // For snd-aloop all four controls should exist. Require them so a
        // kernel/ABI mismatch is caught during installation via --probe.
        let active = loopback_active.ok_or_else(|| {
            app_error(format!(
                "ALSA control '{LOOPBACK_ACTIVE}' not found for device {},{}",
                self.device, self.subdevice
            ))
        })?;
        let rate = loopback_rate.ok_or_else(|| {
            app_error(format!(
                "ALSA control '{LOOPBACK_RATE}' not found for device {},{}",
                self.device, self.subdevice
            ))
        })?;
        let channels = channels.ok_or_else(|| {
            app_error(format!(
                "ALSA control '{LOOPBACK_CHANNELS}' not found for device {},{}",
                self.device, self.subdevice
            ))
        })?;
        let raw_format = raw_format.ok_or_else(|| {
            app_error(format!(
                "ALSA control '{LOOPBACK_FORMAT}' not found for device {},{}",
                self.device, self.subdevice
            ))
        })?;

        let sample_format = alsa_format_to_camilladsp(raw_format)?;
        if sample_format.is_none() {
            log(
                LogLevel::Debug,
                self.log_level,
                format!(
                    "ALSA format value {raw_format} is known but not mapped by the Python controller"
                ),
            );
        }

        Ok(DeviceSnapshot {
            active,
            wave: WaveFormat {
                sample_rate: Some(rate),
                sample_format: sample_format.map(str::to_owned),
                channels: Some(channels),
            },
        })
    }
}

fn nonnegative_u32(value: i32) -> Option<u32> {
    (value >= 0).then_some(value as u32)
}

// Exact Linux sample-format mapping used by HEnquist/camilladsp-controller's
// alsa_listener.py at the commit pinned by the Python installer.
// Known-but-unmapped ALSA formats return None, exactly like the Python helper.
fn alsa_format_to_camilladsp(value: i32) -> AppResult<Option<&'static str>> {
    let mapped = match value {
        2 => Some("S16_LE"),
        6 => Some("S24_4_RJ_LE"),
        10 => Some("S32_LE"),
        14 => Some("F32_LE"),
        16 => Some("F64_LE"),
        32 => Some("S24_3_LE"),

        // Values represented by SampleFormat in the Python controller but not
        // translated by alsa_format_to_cdsp().
        0..=28 | 31..=52 => None,
        _ => {
            return Err(app_error(format!(
                "unknown ALSA sample-format enum value {value}"
            )))
        }
    };
    Ok(mapped)
}

fn yaml_key(name: &str) -> YamlValue {
    YamlValue::String(name.to_owned())
}

fn mapping_mut<'a>(value: &'a mut YamlValue, context: &str) -> AppResult<&'a mut Mapping> {
    value
        .as_mapping_mut()
        .ok_or_else(|| app_error(format!("{context} must be a YAML mapping")))
}

fn mapping<'a>(value: &'a YamlValue, context: &str) -> AppResult<&'a Mapping> {
    value
        .as_mapping()
        .ok_or_else(|| app_error(format!("{context} must be a YAML mapping")))
}

fn yaml_u32(value: &YamlValue) -> Option<u32> {
    value.as_u64().and_then(|v| u32::try_from(v).ok())
}

fn adapt_config(path: &Path, wave: &WaveFormat) -> AppResult<String> {
    // Intentionally resolve/read the path on EVERY adaptation. The installer
    // passes active_config.yml, a stable symlink controlled by CamillaGUI.
    // This is the key piCoreDSP patch over upstream AdaptConfig, which caches
    // the initial file contents.
    let raw = fs::read_to_string(path).map_err(|err| {
        app_error(format!("unable to read config {}: {err}", path.display()))
    })?;
    let mut root: YamlValue = serde_yaml_ng::from_str(&raw)?;

    let root_map = mapping_mut(&mut root, "config root")?;
    let devices_value = root_map
        .get_mut(&yaml_key("devices"))
        .ok_or_else(|| app_error("config has no 'devices' section"))?;
    let devices = mapping_mut(devices_value, "devices")?;

    if let Some(rate) = wave.sample_rate {
        let resampler_key = yaml_key("resampler");
        let has_resampler = devices
            .get(&resampler_key)
            .map(|v| !v.is_null())
            .unwrap_or(false);

        if !has_resampler {
            devices.insert(yaml_key("samplerate"), YamlValue::from(rate as u64));
        } else {
            // Match Python AdaptConfig semantics for a present resampler:
            // it must be a mapping with a string `type`. Valid CamillaDSP
            // configs satisfy this, while malformed configs fail early.
            let resampler = devices
                .get(&resampler_key)
                .ok_or_else(|| app_error("devices.resampler disappeared during adaptation"))?;
            let resampler_map = mapping(resampler, "devices.resampler")?;
            let resampler_type = resampler_map
                .get(&yaml_key("type"))
                .and_then(YamlValue::as_str)
                .ok_or_else(|| app_error("devices.resampler.type must be a string"))?
                .to_owned();

            let configured_rate = devices
                .get(&yaml_key("samplerate"))
                .and_then(yaml_u32);

            devices.insert(
                yaml_key("capture_samplerate"),
                YamlValue::from(rate as u64),
            );

            // Match AdaptConfig._change_sample_rate(): a synchronous 1:1
            // resampler is removed for the runtime copy only.
            if resampler_type == "Synchronous"
                && configured_rate == Some(rate)
            {
                devices.insert(resampler_key, YamlValue::Null);
            }
        }
    }

    let capture_value = devices
        .get_mut(&yaml_key("capture"))
        .ok_or_else(|| app_error("config has no 'devices.capture' section"))?;
    let capture = mapping_mut(capture_value, "devices.capture")?;

    if let Some(format) = wave.sample_format.as_deref() {
        let format_key = yaml_key("format");
        if capture
            .get(&format_key)
            .map(|value| !value.is_null())
            .unwrap_or(false)
        {
            capture.insert(format_key, YamlValue::String(format.to_owned()));
        }
    }

    if let Some(channels) = wave.channels {
        let configured = capture
            .get(&yaml_key("channels"))
            .and_then(yaml_u32)
            .ok_or_else(|| app_error("devices.capture.channels is missing or invalid"))?;
        if configured != channels {
            return Err(app_error(format!(
                "changing capture channels is not implemented (config={configured}, stream={channels})"
            )));
        }
    }

    Ok(serde_yaml_ng::to_string(&root)?)
}

#[derive(Debug)]
enum WsError {
    Transport(String),
    Command(String),
    Protocol(String),
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "CamillaDSP websocket transport error: {msg}"),
            Self::Command(msg) => write!(f, "CamillaDSP command error: {msg}"),
            Self::Protocol(msg) => write!(f, "CamillaDSP websocket protocol error: {msg}"),
        }
    }
}

impl Error for WsError {}

struct CamillaWs {
    socket: WsSocket,
}

impl CamillaWs {
    fn connect(host: &str, port: u16) -> Result<Self, WsError> {
        let url = format!("ws://{host}:{port}");
        let (socket, _) = connect(url)
            .map_err(|err| WsError::Transport(format!("connect failed: {err}")))?;
        let mut client = Self { socket };
        // pyCamillaDSP performs GetVersion immediately after connecting.
        let _ = client.query("GetVersion", None)?;
        Ok(client)
    }

    fn query(
        &mut self,
        command: &str,
        argument: Option<JsonValue>,
    ) -> Result<Option<JsonValue>, WsError> {
        let request = match argument {
            Some(argument) => {
                let mut object = serde_json::Map::new();
                object.insert(command.to_owned(), argument);
                JsonValue::Object(object)
            }
            None => JsonValue::String(command.to_owned()),
        };
        let serialized = serde_json::to_string(&request)
            .map_err(|err| WsError::Protocol(format!("request JSON: {err}")))?;

        self.socket
            .send(Message::text(serialized))
            .map_err(|err| WsError::Transport(format!("send failed: {err}")))?;

        loop {
            let message = self
                .socket
                .read()
                .map_err(|err| WsError::Transport(format!("read failed: {err}")))?;

            match message {
                Message::Text(text) => {
                    let reply: JsonValue = serde_json::from_str(text.as_str())
                        .map_err(|err| WsError::Protocol(format!("invalid JSON reply: {err}")))?;
                    return parse_ws_reply(command, reply);
                }
                Message::Close(_) => {
                    return Err(WsError::Transport(
                        "connection closed while waiting for reply".to_owned(),
                    ))
                }
                Message::Ping(_) | Message::Pong(_) => {
                    // tungstenite queues protocol responses automatically.
                    let _ = self.socket.flush();
                }
                Message::Binary(_) | Message::Frame(_) => {}
            }
        }
    }

    fn close(&mut self) {
        let _ = self.socket.close(None);
    }
}

impl Drop for CamillaWs {
    fn drop(&mut self) {
        self.close();
    }
}

fn parse_ws_reply(command: &str, reply: JsonValue) -> Result<Option<JsonValue>, WsError> {
    let entry = reply.get(command).ok_or_else(|| {
        WsError::Protocol(format!("reply does not contain command '{command}': {reply}"))
    })?;

    if let Some(error) = entry.get("error") {
        return Err(WsError::Command(error.to_string()));
    }

    let result = entry.get("result").ok_or_else(|| {
        WsError::Protocol(format!("reply for '{command}' has no result: {entry}"))
    })?;

    match result {
        JsonValue::String(value) if value == "Ok" => Ok(entry.get("value").cloned()),
        JsonValue::String(value) => Err(WsError::Command(value.clone())),
        JsonValue::Object(values) => {
            let message = values
                .iter()
                .next()
                .map(|(kind, message)| format!("{kind}: {message}"))
                .unwrap_or_else(|| "empty error result".to_owned());
            Err(WsError::Command(message))
        }
        other => Err(WsError::Protocol(format!(
            "invalid result for '{command}': {other}"
        ))),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProcessingState {
    Running,
    Paused,
    Inactive,
    Starting,
    Stalled,
    Unknown(String),
}

fn parse_processing_state(value: Option<JsonValue>) -> Result<ProcessingState, WsError> {
    let value = value.ok_or_else(|| WsError::Protocol("GetState returned no value".to_owned()))?;
    let state = value
        .as_str()
        .ok_or_else(|| WsError::Protocol(format!("GetState returned non-string: {value}")))?;
    Ok(match state {
        "Running" => ProcessingState::Running,
        "Paused" => ProcessingState::Paused,
        "Inactive" => ProcessingState::Inactive,
        "Starting" => ProcessingState::Starting,
        "Stalled" => ProcessingState::Stalled,
        other => ProcessingState::Unknown(other.to_owned()),
    })
}

#[derive(Clone, Debug, PartialEq)]
enum StopReason {
    None,
    Done,
    CaptureError(String),
    PlaybackError(String),
    UnknownError(String),
    CaptureFormatChange(u32),
    PlaybackFormatChange(u32),
    Unknown(JsonValue),
}

fn json_payload_string(value: &JsonValue) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn parse_stop_reason(value: Option<JsonValue>) -> Result<StopReason, WsError> {
    let value = value.ok_or_else(|| WsError::Protocol("GetStopReason returned no value".to_owned()))?;
    match &value {
        JsonValue::String(reason) => Ok(match reason.as_str() {
            "None" => StopReason::None,
            "Done" => StopReason::Done,
            other => StopReason::Unknown(JsonValue::String(other.to_owned())),
        }),
        JsonValue::Object(values) if values.len() == 1 => {
            let (reason, data) = values.iter().next().expect("length checked");
            Ok(match reason.as_str() {
                "CaptureError" => StopReason::CaptureError(json_payload_string(data)),
                "PlaybackError" => StopReason::PlaybackError(json_payload_string(data)),
                "UnknownError" => StopReason::UnknownError(json_payload_string(data)),
                "CaptureFormatChange" => StopReason::CaptureFormatChange(
                    data.as_u64()
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(0),
                ),
                "PlaybackFormatChange" => StopReason::PlaybackFormatChange(
                    data.as_u64()
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(0),
                ),
                _ => StopReason::Unknown(value.clone()),
            })
        }
        _ => Ok(StopReason::Unknown(value)),
    }
}

struct Controller {
    client: CamillaWs,
    listener: AlsaLoopbackListener,
    adapt_path: PathBuf,
    fallback_wave: WaveFormat,
    config: Option<String>,
    error_on_start: bool,
    log_level: LogLevel,
}

impl Controller {
    fn new(args: &Args) -> AppResult<(Self, DeviceSnapshot)> {
        let listener = AlsaLoopbackListener::new(&args.device, args.log_level)?;
        let initial = listener.read_snapshot()?;
        let client = CamillaWs::connect(&args.host, args.port)?;

        let fallback_wave = WaveFormat {
            sample_rate: args.initial_rate,
            sample_format: args.initial_format.clone(),
            channels: args.initial_channels,
        };

        let mut controller = Self {
            client,
            listener,
            adapt_path: args
                .adapt
                .clone()
                .ok_or_else(|| app_error("--adapt is required in controller mode"))?,
            fallback_wave,
            config: None,
            error_on_start: false,
            log_level: args.log_level,
        };

        // piCoreDSP extension over upstream behavior: adapt to the actual
        // current loopback rate+format+channels immediately, before the first
        // processing start.
        let effective = initial.wave.with_fallback(&controller.fallback_wave);
        controller.refresh_config(&effective);
        Ok((controller, initial))
    }

    fn refresh_config(&mut self, wave: &WaveFormat) {
        log(
            LogLevel::Info,
            self.log_level,
            format!("Getting new config for {wave}"),
        );
        match adapt_config(&self.adapt_path, wave) {
            Ok(config) => {
                self.config = Some(config);
                log(LogLevel::Info, self.log_level, "Using new config from Adapt provider");
            }
            Err(err) => {
                self.config = None;
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Adapt provider cannot supply config: {err}"),
                );
            }
        }
    }

    fn stop_cdsp(&mut self) -> AppResult<()> {
        log(LogLevel::Info, self.log_level, "Stopping CamillaDSP");
        self.client.query("Stop", None)?;
        self.error_on_start = false;
        Ok(())
    }

    fn start_cdsp(&mut self) -> AppResult<()> {
        let Some(config) = self.config.clone() else {
            log(
                LogLevel::Warning,
                self.log_level,
                "No config available, ignoring start request",
            );
            return Ok(());
        };

        log(
            LogLevel::Info,
            self.log_level,
            "Starting CamillaDSP with new config",
        );

        match self
            .client
            .query("SetConfig", Some(JsonValue::String(config)))
        {
            Ok(_) => {
                self.error_on_start = false;
                Ok(())
            }
            Err(WsError::Command(err)) => {
                // Match Python's CamillaError handling: a bad config/device is
                // remembered and is not retried continuously until a new event.
                self.error_on_start = true;
                log(
                    LogLevel::Error,
                    self.log_level,
                    format!("Unable to start CamillaDSP: {err}"),
                );
                Ok(())
            }
            Err(err) => Err(Box::new(err)),
        }
    }

    fn handle_started(&mut self, snapshot: &DeviceSnapshot) -> AppResult<()> {
        // The Python listener attaches a snapshot to STARTED and the controller
        // then re-reads the controls. Use the fresh snapshot we just read after
        // the 50 ms debounce.
        let effective = snapshot.wave.with_fallback(&self.fallback_wave);
        log(
            LogLevel::Info,
            self.log_level,
            format!("Device started with wave format {effective}"),
        );
        self.refresh_config(&effective);
        self.stop_cdsp()?;
        self.start_cdsp()
    }

    fn process_inactive_state(&mut self, snapshot: &DeviceSnapshot) -> AppResult<()> {
        let reason = parse_stop_reason(self.client.query("GetStopReason", None)?)?;
        match reason {
            StopReason::CaptureFormatChange(reported_rate) if !self.error_on_start => {
                log(
                    LogLevel::Info,
                    self.log_level,
                    format!(
                        "CamillaDSP stopped because capture format changed (reported rate {reported_rate})"
                    ),
                );
                let current = self.listener.read_snapshot()?;
                let mut effective = current.wave.with_fallback(&self.fallback_wave);
                if effective.sample_rate.unwrap_or(0) == 0 && reported_rate > 0 {
                    effective.sample_rate = Some(reported_rate);
                }
                if effective.sample_rate.unwrap_or(0) > 0 {
                    self.refresh_config(&effective);
                    self.stop_cdsp()?;
                    self.start_cdsp()?;
                } else {
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        "Sample rate changed but the new value is unknown",
                    );
                }
            }
            StopReason::Done => {
                log(LogLevel::Debug, self.log_level, "Capture is done, no action");
            }
            StopReason::None => {
                log(LogLevel::Debug, self.log_level, "Initial/inactive state");
                if snapshot.active && !self.error_on_start {
                    self.start_cdsp()?;
                }
            }
            StopReason::CaptureError(message) if !self.error_on_start => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Stopped due to capture error, trying restart: {message}"),
                );
                self.start_cdsp()?;
            }
            StopReason::PlaybackError(message) if !self.error_on_start => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Stopped due to playback error, trying restart: {message}"),
                );
                self.start_cdsp()?;
            }
            StopReason::PlaybackFormatChange(rate) => {
                log(
                    LogLevel::Info,
                    self.log_level,
                    format!("Playback format changed (reported rate {rate})"),
                );
            }
            StopReason::UnknownError(message) => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("CamillaDSP stopped with unknown error: {message}"),
                );
            }
            StopReason::Unknown(value) => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Unknown CamillaDSP stop reason: {value}"),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn run(mut self, mut previous: DeviceSnapshot) -> AppResult<()> {
        log(LogLevel::Info, self.log_level, "Starting ALSA loopback controller");
        loop {
            if self.listener.wait_for_event(CONTROL_LOOP_MS)? {
                thread::sleep(Duration::from_millis(ALSA_DEBOUNCE_MS));
                self.listener.handle_events()?;
            }

            let current = self.listener.read_snapshot()?;

            if !previous.active && current.active {
                self.handle_started(&current)?;
            } else if previous.active && !current.active {
                log(LogLevel::Info, self.log_level, "Device stopped");
                self.stop_cdsp()?;
            } else if previous.active && current.active && previous.wave != current.wave {
                // Python listener emits STOPPED then STARTED for this transition.
                log(
                    LogLevel::Info,
                    self.log_level,
                    format!("Device wave format changed to {}", current.wave),
                );
                self.stop_cdsp()?;
                let effective = current.wave.with_fallback(&self.fallback_wave);
                self.refresh_config(&effective);
                self.start_cdsp()?;
            }

            previous = current.clone();

            let state = parse_processing_state(self.client.query("GetState", None)?)?;
            if state == ProcessingState::Inactive {
                self.process_inactive_state(&current)?;
            } else if let ProcessingState::Unknown(value) = state {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Unknown CamillaDSP processing state: {value}"),
                );
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Args {
    host: String,
    port: u16,
    device: String,
    adapt: Option<PathBuf>,
    initial_rate: Option<u32>,
    initial_format: Option<String>,
    initial_channels: Option<u32>,
    log_level: LogLevel,
    mode: Mode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Run,
    Probe,
    WsCheck,
    WsValidate,
    AdaptCheck,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            host: "localhost".to_owned(),
            port: 1234,
            device: "hw:Loopback,0".to_owned(),
            adapt: None,
            initial_rate: None,
            initial_format: None,
            initial_channels: None,
            log_level: LogLevel::Info,
            mode: Mode::Run,
        }
    }
}

fn usage() {
    println!(
        "picoredsp-controller {VERSION}\n\
Rust ALSA loopback controller for CamillaDSP\n\n\
Usage:\n\
  picoredsp-controller --adapt PATH [options]\n\
  picoredsp-controller --probe [--device DEVICE]\n\
  picoredsp-controller --ws-check [--host HOST] [--port PORT]\n\
  picoredsp-controller --ws-validate --adapt PATH [--host HOST] [--port PORT]\n\
  picoredsp-controller --adapt-check --adapt PATH [--rate R --format F --channels N]\n\n\
Options:\n\
  -a, --adapt PATH       Active config path/symlink to adapt\n\
  -d, --device DEVICE    ALSA control device (default: hw:Loopback,0)\n\
      --host HOST        CamillaDSP websocket host (default: localhost)\n\
  -p, --port PORT        CamillaDSP websocket port (default: 1234)\n\
  -r, --rate RATE        Initial fallback sample rate\n\
  -f, --format FORMAT    Initial fallback CamillaDSP sample format\n\
  -c, --channels N       Initial fallback capture channel count\n\
  -l, --log-level LEVEL  DEBUG, INFO, WARNING, ERROR, CRITICAL\n\
      --probe            Read snd-aloop controls once and exit\n\
      --ws-check         Connect, query CamillaDSP version, close, exit\n\
      --ws-validate      Adapt YAML and ValidateConfig over websocket\n\
      --adapt-check      Adapt YAML once, write result to stdout, exit\n\
  -h, --help             Show this help\n\
  -V, --version          Show version"
    );
}

fn parse_args() -> AppResult<Option<Args>> {
    let mut args = Args::default();
    let mut iter = env::args().skip(1);

    while let Some(arg) = iter.next() {
        let mut next_value = |name: &str| -> AppResult<String> {
            iter.next()
                .ok_or_else(|| app_error(format!("{name} requires a value")))
        };

        match arg.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("picoredsp-controller {VERSION}");
                return Ok(None);
            }
            "--probe" => args.mode = Mode::Probe,
            "--ws-check" => args.mode = Mode::WsCheck,
            "--ws-validate" => args.mode = Mode::WsValidate,
            "--adapt-check" => args.mode = Mode::AdaptCheck,
            "--host" => args.host = next_value("--host")?,
            "-p" | "--port" => {
                args.port = next_value("--port")?
                    .parse()
                    .map_err(|_| app_error("--port must be an integer from 1 to 65535"))?;
            }
            "-d" | "--device" => args.device = next_value("--device")?,
            "-a" | "--adapt" => args.adapt = Some(PathBuf::from(next_value("--adapt")?)),
            "-r" | "--rate" => {
                args.initial_rate = Some(
                    next_value("--rate")?
                        .parse()
                        .map_err(|_| app_error("--rate must be a positive integer"))?,
                );
            }
            "-f" | "--format" => args.initial_format = Some(next_value("--format")?),
            "-c" | "--channels" => {
                args.initial_channels = Some(
                    next_value("--channels")?
                        .parse()
                        .map_err(|_| app_error("--channels must be a positive integer"))?,
                );
            }
            "-l" | "--log-level" => {
                args.log_level = LogLevel::parse(&next_value("--log-level")?)?;
            }
            other => return Err(app_error(format!("unknown argument: {other}"))),
        }
    }

    if matches!(args.mode, Mode::Run | Mode::WsValidate | Mode::AdaptCheck) && args.adapt.is_none() {
        return Err(app_error("this mode requires --adapt PATH"));
    }
    Ok(Some(args))
}

fn run_main() -> AppResult<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };

    match args.mode {
        Mode::Probe => {
            let listener = AlsaLoopbackListener::new(&args.device, args.log_level)?;
            let snapshot = listener.read_snapshot()?;
            println!(
                "active={} rate={} format={} channels={}",
                snapshot.active,
                snapshot
                    .wave
                    .sample_rate
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                snapshot
                    .wave
                    .sample_format
                    .as_deref()
                    .unwrap_or("unknown"),
                snapshot
                    .wave
                    .channels
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            );
            Ok(())
        }
        Mode::WsCheck => {
            let mut client = CamillaWs::connect(&args.host, args.port)?;
            let version = client.query("GetVersion", None)?;
            println!(
                "CamillaDSP websocket OK, version={}",
                version
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".to_owned())
            );
            client.close();
            Ok(())
        }
        Mode::WsValidate => {
            let wave = WaveFormat {
                sample_rate: args.initial_rate,
                sample_format: args.initial_format.clone(),
                channels: args.initial_channels,
            };
            let adapted = adapt_config(args.adapt.as_deref().expect("validated"), &wave)?;
            let mut client = CamillaWs::connect(&args.host, args.port)?;
            let _ = client.query("ValidateConfig", Some(JsonValue::String(adapted)))?;
            println!("CamillaDSP websocket ValidateConfig OK");
            client.close();
            Ok(())
        }
        Mode::AdaptCheck => {
            let wave = WaveFormat {
                sample_rate: args.initial_rate,
                sample_format: args.initial_format.clone(),
                channels: args.initial_channels,
            };
            let adapted = adapt_config(args.adapt.as_deref().expect("validated"), &wave)?;
            print!("{adapted}");
            Ok(())
        }
        Mode::Run => {
            let (controller, initial) = Controller::new(&args)?;
            controller.run(initial)
        }
    }
}

fn main() {
    if let Err(err) = run_main() {
        eprintln!("ERROR - {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!("picoredsp-{name}-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn base_config(playback: &str, format: Option<&str>) -> String {
        let format_line = format
            .map(|fmt| format!("    format: {fmt}\n"))
            .unwrap_or_default();
        format!(
            "devices:\n  samplerate: 44100\n  chunksize: 2048\n  capture:\n    type: Alsa\n    channels: 2\n    device: \"hw:Loopback,0,0\"\n{format_line}  playback:\n    type: Alsa\n    channels: 2\n    device: \"{playback}\"\nfilters: {{}}\nmixers: {{}}\npipeline: []\nprocessors: {{}}\n"
        )
    }

    #[test]
    fn sample_format_mapping_matches_python_controller() {
        assert_eq!(alsa_format_to_camilladsp(2).unwrap(), Some("S16_LE"));
        assert_eq!(alsa_format_to_camilladsp(6).unwrap(), Some("S24_4_RJ_LE"));
        assert_eq!(alsa_format_to_camilladsp(10).unwrap(), Some("S32_LE"));
        assert_eq!(alsa_format_to_camilladsp(14).unwrap(), Some("F32_LE"));
        assert_eq!(alsa_format_to_camilladsp(16).unwrap(), Some("F64_LE"));
        assert_eq!(alsa_format_to_camilladsp(32).unwrap(), Some("S24_3_LE"));
        assert_eq!(alsa_format_to_camilladsp(0).unwrap(), None);
        assert!(alsa_format_to_camilladsp(99).is_err());
    }

    #[test]
    fn adapt_updates_rate_format_and_keeps_playback() {
        let dir = test_dir("adapt");
        let config = dir.join("config.yml");
        fs::write(&config, base_config("hw:99,0", Some("S16_LE"))).unwrap();
        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: Some("S32_LE".to_owned()),
            channels: Some(2),
        };
        let adapted = adapt_config(&config, &wave).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();
        assert_eq!(parsed["devices"]["samplerate"].as_u64(), Some(48000));
        assert_eq!(
            parsed["devices"]["capture"]["format"].as_str(),
            Some("S32_LE")
        );
        assert_eq!(
            parsed["devices"]["playback"]["device"].as_str(),
            Some("hw:99,0")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn automatic_capture_format_stays_automatic() {
        let dir = test_dir("autoformat");
        let config = dir.join("config.yml");
        fs::write(&config, base_config("null", None)).unwrap();
        let wave = WaveFormat {
            sample_rate: Some(96000),
            sample_format: Some("S32_LE".to_owned()),
            channels: Some(2),
        };
        let adapted = adapt_config(&config, &wave).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();
        assert!(parsed["devices"]["capture"].get("format").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn active_symlink_is_reread_every_adaptation() {
        let dir = test_dir("symlink");
        let first = dir.join("first.yml");
        let second = dir.join("second.yml");
        let active = dir.join("active.yml");
        fs::write(&first, base_config("null", Some("S16_LE"))).unwrap();
        fs::write(&second, base_config("hw:99,0", Some("S16_LE"))).unwrap();
        symlink(&first, &active).unwrap();

        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: Some("S32_LE".to_owned()),
            channels: Some(2),
        };
        let first_adapted = adapt_config(&active, &wave).unwrap();
        let first_parsed: YamlValue = serde_yaml_ng::from_str(&first_adapted).unwrap();
        assert_eq!(
            first_parsed["devices"]["playback"]["device"].as_str(),
            Some("null")
        );

        fs::remove_file(&active).unwrap();
        symlink(&second, &active).unwrap();
        let second_adapted = adapt_config(&active, &wave).unwrap();
        let second_parsed: YamlValue = serde_yaml_ng::from_str(&second_adapted).unwrap();
        assert_eq!(
            second_parsed["devices"]["playback"]["device"].as_str(),
            Some("hw:99,0")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn synchronous_one_to_one_resampler_is_disabled() {
        let dir = test_dir("resampler");
        let config = dir.join("config.yml");
        fs::write(
            &config,
            "devices:\n  samplerate: 48000\n  resampler:\n    type: Synchronous\n  capture:\n    type: Alsa\n    channels: 2\n    device: \"hw:Loopback,0,0\"\n  playback:\n    type: Alsa\n    channels: 2\n    device: \"null\"\nfilters: {}\nmixers: {}\npipeline: []\nprocessors: {}\n",
        )
        .unwrap();
        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: None,
            channels: Some(2),
        };
        let adapted = adapt_config(&config, &wave).unwrap();
        let parsed: YamlValue = serde_yaml_ng::from_str(&adapted).unwrap();
        assert!(parsed["devices"]["resampler"].is_null());
        assert_eq!(
            parsed["devices"]["capture_samplerate"].as_u64(),
            Some(48000)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_resampler_is_rejected_like_python_adapt_config() {
        let dir = test_dir("bad-resampler");
        let config = dir.join("config.yml");
        fs::write(
            &config,
            "devices:\n  samplerate: 48000\n  resampler: {}\n  capture:\n    type: Alsa\n    channels: 2\n    device: \"hw:Loopback,0,0\"\n  playback:\n    type: Alsa\n    channels: 2\n    device: \"null\"\nfilters: {}\nmixers: {}\npipeline: []\nprocessors: {}\n",
        )
        .unwrap();
        let wave = WaveFormat {
            sample_rate: Some(48000),
            sample_format: None,
            channels: Some(2),
        };
        assert!(adapt_config(&config, &wave).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn channel_change_is_rejected_like_python_adapt_config() {
        let dir = test_dir("channels");
        let config = dir.join("config.yml");
        fs::write(&config, base_config("null", Some("S16_LE"))).unwrap();
        let wave = WaveFormat {
            sample_rate: Some(44100),
            sample_format: Some("S16_LE".to_owned()),
            channels: Some(6),
        };
        assert!(adapt_config(&config, &wave).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn camilladsp_stop_reason_shape_matches_v4_protocol() {
        assert_eq!(
            parse_stop_reason(Some(serde_json::json!({"CaptureFormatChange": 96000}))).unwrap(),
            StopReason::CaptureFormatChange(96000)
        );
        assert_eq!(
            parse_stop_reason(Some(serde_json::json!({"CaptureError": "boom"}))).unwrap(),
            StopReason::CaptureError("boom".to_owned())
        );
        assert_eq!(
            parse_stop_reason(Some(serde_json::json!("None"))).unwrap(),
            StopReason::None
        );
    }

    #[test]
    fn websocket_reply_shape_matches_camilladsp() {
        let reply = serde_json::json!({"GetState": {"result": "Ok", "value": "Inactive"}});
        let value = parse_ws_reply("GetState", reply).unwrap();
        assert_eq!(value, Some(serde_json::json!("Inactive")));

        let err = serde_json::json!({
            "SetConfig": {
                "result": {"ConfigValidationError": "bad config"}
            }
        });
        assert!(matches!(
            parse_ws_reply("SetConfig", err),
            Err(WsError::Command(_))
        ));
    }

    #[test]
    fn mapping_helpers_accept_normal_camilladsp_yaml() {
        let value: YamlValue = serde_yaml_ng::from_str(&base_config("null", None)).unwrap();
        let root = mapping(&value, "root").unwrap();
        assert!(root.contains_key(&yaml_key("devices")));
    }
}
PICORE_RUST_SOURCE

cat > "${RUST_PROJECT}/README.md" <<'PICORE_RUST_README'
# piCoreDSP Rust controller

This is the Rust replacement for the Python runtime used by the piCoreDSP
`snd-aloop` architecture.

It intentionally ports the Linux `--adapt` behavior used by piCoreDSP rather
than every platform/provider in the generic Python `camilladsp-controller`.

Runtime responsibilities:

1. Monitor the ALSA `snd-aloop` HCTL controls `PCM Slave Active`, `PCM Slave
   Rate`, `PCM Slave Format`, and `PCM Slave Channels`.
2. Debounce ALSA control events by 50 ms, matching the Python listener.
3. Re-read the CamillaGUI-managed `active_config.yml` symlink for every
   adaptation.
4. Adapt sample rate, explicit capture format, and validate channel count using
   the same rules as Python `AdaptConfig`.
5. Control CamillaDSP over its v4.1.3 WebSocket protocol (`GetState`,
   `GetStopReason`, `Stop`, and `SetConfig`).
6. Exit on transport/ALSA failures so the Tiny Core boot supervisor can restart
   it.

Useful diagnostics:

```sh
picoredsp-controller --probe --device hw:Loopback,0
picoredsp-controller --ws-check --host 127.0.0.1 --port 1234
```

Normal piCoreDSP invocation:

```sh
picoredsp-controller \
  --host 127.0.0.1 \
  --port 1234 \
  --device hw:Loopback,0 \
  --adapt /mnt/mmcblk0p2/tce/camilladsp/active_config.yml \
  --log-level INFO
```
PICORE_RUST_README

###############################################################################
# Compile and unit-test Rust controller
###############################################################################

cd "${RUST_PROJECT}"

# Resolve dependency versions once for this installation, then use the lockfile
# for every test/build command in the same transaction.
cargo generate-lockfile
cargo test --locked
cargo build --release --locked

if [ ! -x "${RUST_PROJECT}/target/release/picoredsp-controller" ]; then
    echo "ERROR: Rust controller binary was not produced."
    exit 1
fi

mkdir -p "${BUILD_DIR}/usr/local/bin" "${RUST_SOURCE_DIR}"
cp -f "${RUST_PROJECT}/target/release/picoredsp-controller" "${RUST_RUNTIME_BIN}"
chmod 755 "${RUST_RUNTIME_BIN}"

# Catch target-side dynamic-link failures before the transactional commit.
# The Rust ALSA wrapper is intentionally a thin alsa-lib binding.
if command -v ldd >/dev/null 2>&1; then
    if ldd "${RUST_RUNTIME_BIN}" 2>&1 | grep -q 'not found'; then
        echo "ERROR: Rust controller has unresolved runtime libraries:"
        ldd "${RUST_RUNTIME_BIN}" 2>&1 || true
        exit 1
    fi
fi

# Keep exact build inputs (including the generated Cargo.lock) in the TCZ for
# auditing/reproducibility. They are not needed at runtime.
cp -f "${RUST_PROJECT}/Cargo.toml" "${RUST_SOURCE_DIR}/Cargo.toml"
cp -f "${RUST_PROJECT}/Cargo.lock" "${RUST_SOURCE_DIR}/Cargo.lock"
cp -f "${RUST_PROJECT}/src/main.rs" "${RUST_SOURCE_DIR}/main.rs"
cp -f "${RUST_PROJECT}/README.md" "${RUST_SOURCE_DIR}/README.md"

"${RUST_RUNTIME_BIN}" --help >/dev/null

# Verify the binary can open the exact snd-aloop HCTL device used at runtime,
# find the expected PCM controls and read their values on this kernel.
echo "Probing snd-aloop controls with Rust controller..."
"${RUST_RUNTIME_BIN}" --probe --device hw:Loopback,0

# Verify the piCoreDSP-specific adaptation behavior on the ACTUAL staged
# Bypass config: symlink/file parsing, rate update and channel validation.
RUST_ADAPTED_TEST="/tmp/picoredsp-rust-adapted.$$.yml"
"${RUST_RUNTIME_BIN}"     --adapt-check     --adapt "${STAGE_BYPASS_CONFIG}"     --rate 48000     --format S32_LE     --channels 2     > "${RUST_ADAPTED_TEST}"

if ! grep -Eq '^[[:space:]]*samplerate:[[:space:]]*48000[[:space:]]*$' "${RUST_ADAPTED_TEST}"; then
    echo "ERROR: Rust controller did not adapt samplerate to 48000."
    exit 1
fi

if [ "$(read_cdsp_playback_device "${RUST_ADAPTED_TEST}" 2>/dev/null || true)" != "${PLAYBACK_DEVICE}" ]; then
    echo "ERROR: Rust adaptation did not preserve the selected playback device."
    exit 1
fi

rm -f "${RUST_ADAPTED_TEST}"

echo "Rust controller compile, unit tests and ALSA probe OK."

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
sudo rm -f "${ACTIVE_CONFIG_LINK}"
sudo ln -s "${BYPASS_CONFIG}" "${ACTIVE_CONFIG_LINK}"

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
echo "  ${BYPASS_CONFIG}"
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
