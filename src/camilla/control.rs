//! [`CamillaControl`] trait and its companion types (roadmap §23).
//!
//! The reconciler (and everything above the WebSocket layer) communicates with
//! CamillaDSP exclusively through this trait.  No wire-format details — JSON
//! command names, response envelope shapes, version-specific quirks — ever appear
//! outside `camilla/protocol_v4.rs` or `camilla/protocol_v5.rs`.
//!
//! [`CamillaStateEvents`] is the optional push-based state-change subscription
//! (available in CamillaDSP 4.2+).  When it is unavailable, the reconciler falls
//! back to the polling path via [`DspTriggerSource`] in `rate_sync/`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;

use crate::{camilla::config_document::ConfigDocument, error::PicorecdspError};

// ── DSP process state ────────────────────────────────────────────────────────

/// The set of states the CamillaDSP process can be in, as observed through the
/// WebSocket API (roadmap §8, State Truth 2).
///
/// Rust never invents or caches this; it is always read fresh from CamillaDSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DspState {
    /// The engine is in the process of starting up.
    Starting,
    /// The engine is actively processing audio.
    Running,
    /// The engine is paused (capture stopped, DSP suspended).
    Paused,
    /// The engine stopped normally — no capture is active, waiting for a
    /// new source.  `GetPreviousConfig` is available after this state.
    #[default]
    Inactive,
    /// The engine has stalled (e.g. a format change that it could not handle
    /// automatically).
    Stalled,
    /// The engine encountered a fatal error and has stopped.
    Failed,
}

impl DspState {
    /// Whether this state represents a transitional/unstable condition that the
    /// reconciler should wait out before taking action.
    pub fn is_transitional(&self) -> bool {
        matches!(self, DspState::Starting)
    }

    /// Whether the engine is currently producing audio (Running or Paused).
    pub fn is_active(&self) -> bool {
        matches!(self, DspState::Running | DspState::Paused)
    }

    /// Whether the DSP is settled and inactive — i.e. `GetPreviousConfig` should
    /// be authoritative.
    pub fn is_settled_inactive(&self) -> bool {
        *self == DspState::Inactive
    }
}

// ── Stop reason ─────────────────────────────────────────────────────────────

/// Why CamillaDSP stopped (from `GetStopReason`).
///
/// Rust uses this to classify failures before deciding whether to retry or wait
/// for user intervention (roadmap §33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Capture stream produced an error (e.g. snd-aloop capture device gone away).
    CaptureError,
    /// Playback device produced an error.
    PlaybackError,
    /// The capture stream format changed — CamillaDSP stopped itself.
    CaptureFormatChange,
    /// The playback stream format changed.
    PlaybackFormatChange,
    /// `stop_on_inactive` fired: the capture stream went silent.
    Done,
    /// No stop reason is recorded (engine never started, or reason unknown).
    None,
    /// Any other string value returned by CamillaDSP that this crate does not
    /// enumerate yet — carried as-is for logging/diagnostics.
    Other(String),
}

// ── Version ──────────────────────────────────────────────────────────────────

/// Semantic version of the running CamillaDSP instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Whether this version supports `SubscribeState` push events (4.2+).
    pub fn supports_subscribe_state(&self) -> bool {
        (self.major, self.minor) >= (4, 2)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Parse a `"major.minor.patch"` version string.
impl std::str::FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.trim().split('.').collect();
        if parts.len() < 2 {
            return Err(format!("cannot parse version string `{s}`"));
        }
        let parse = |p: &str| {
            p.parse::<u32>()
                .map_err(|_| format!("invalid version component `{p}` in `{s}`"))
        };
        Ok(Version {
            major: parse(parts[0])?,
            minor: parse(parts[1])?,
            patch: if parts.len() >= 3 {
                parse(parts[2])?
            } else {
                0
            },
        })
    }
}

// ── CamillaControl trait ──────────────────────────────────────────────────────

/// The semantic API that the reconciler uses to talk to CamillaDSP (roadmap §23).
///
/// Every method results in exactly one WebSocket round-trip to CamillaDSP and
/// returns a fresh result.  Nothing is cached at this layer — the reconciler is
/// responsible for deciding when to read and when to act.
///
/// # Object-safety
///
/// This trait uses `async-trait` so it can be used as `dyn CamillaControl` in
/// the reconciler, allowing protocol_v4 and protocol_v5 to be swapped without
/// recompiling the reconciler.
#[async_trait]
pub trait CamillaControl: Send + Sync {
    /// Return the version of the running CamillaDSP instance.
    async fn version(&self) -> Result<Version, PicorecdspError>;

    /// Return the current DSP process state (fresh read, no cache).
    async fn state(&self) -> Result<DspState, PicorecdspError>;

    /// Return the most recent stop reason, or `None` if CamillaDSP has not
    /// stopped or has no recorded reason.
    async fn stop_reason(&self) -> Result<Option<StopReason>, PicorecdspError>;

    /// Return the currently applied runtime config (roadmap §14, State Truth 3).
    /// Returns `Ok(None)` when CamillaDSP has no active config (e.g. just started
    /// without a statefile and no prior `SetConfig`).
    async fn active_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError>;

    /// Return the config from the most recent `Inactive` transition, i.e. the
    /// last config that was applied before `stop_on_inactive` fired (roadmap §14,
    /// §17).  Returns `Ok(None)` when no previous config is recorded.
    async fn previous_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError>;

    /// Return the filesystem path CamillaDSP was started with (roadmap §14, State
    /// Truth 4).  Returns `Ok(None)` if CamillaDSP has no config file path set.
    async fn config_file_path(&self) -> Result<Option<PathBuf>, PicorecdspError>;

    /// Apply `config` as the new running config.  Equivalent to a GUI `Apply`
    /// from CamillaDSP's perspective.  Rust calls this only for a rate-only patch:
    /// it passes the existing config with exactly one rate field changed.
    async fn set_config(&self, config: &ConfigDocument) -> Result<(), PicorecdspError>;

    /// Patch a single named field in the running config (e.g. `"devices.samplerate"`)
    /// to `value`, without touching anything else.  This is the preferred rate-sync
    /// path when CamillaDSP is Running/Paused (roadmap §16).
    async fn set_config_value(&self, path: &str, value: Value) -> Result<(), PicorecdspError>;

    /// Send a `Stop` command.  Used only as a last-resort safety recovery
    /// (roadmap §12, §33) — NOT called on a normal producer-stop.
    async fn stop(&self) -> Result<(), PicorecdspError>;
}

// ── CamillaStateEvents trait ─────────────────────────────────────────────────

/// Optional push-based state-change subscription (CamillaDSP 4.2+ / 5.x).
///
/// When available, the reconciler uses this to wake immediately on a state
/// change rather than polling.  When unavailable, it falls back to the polling
/// [`DspTriggerSource`] defined in `rate_sync/`.
///
/// **Removal criterion (Cliffhanger E):** once the production baseline reliably
/// supports `SubscribeState`, the fast-polling `DspTriggerSource` implementation
/// can be deleted.  The slow safety-reconcile stays.  See `upstream/capabilities.yml`.
#[async_trait]
pub trait CamillaStateEvents: Send + Sync {
    /// Subscribe to state-change events.  Returns a channel receiver that yields
    /// `DspState` values as CamillaDSP transitions between states.
    ///
    /// The receiver may be dropped to unsubscribe.  If the WebSocket disconnects
    /// the receiver will be closed; the caller must re-subscribe after reconnect.
    async fn subscribe_state(
        &self,
    ) -> Result<tokio::sync::mpsc::Receiver<DspState>, PicorecdspError>;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsp_state_transitional_and_active_flags() {
        assert!(DspState::Starting.is_transitional());
        assert!(!DspState::Running.is_transitional());

        assert!(DspState::Running.is_active());
        assert!(DspState::Paused.is_active());
        assert!(!DspState::Inactive.is_active());
        assert!(!DspState::Stalled.is_active());
        assert!(!DspState::Failed.is_active());

        assert!(DspState::Inactive.is_settled_inactive());
        assert!(!DspState::Running.is_settled_inactive());
    }

    #[test]
    fn version_parsing() {
        let v: Version = "4.2.1".parse().unwrap();
        assert_eq!(v, Version::new(4, 2, 1));
        assert!(v.supports_subscribe_state());

        let v: Version = "4.1.0".parse().unwrap();
        assert!(!v.supports_subscribe_state());

        let v: Version = "5.0.0".parse().unwrap();
        assert!(v.supports_subscribe_state());
    }

    #[test]
    fn version_parsing_short_form() {
        let v: Version = "4.2".parse().unwrap();
        assert_eq!(v.patch, 0);
        assert_eq!(v.major, 4);
    }

    #[test]
    fn version_parsing_invalid() {
        assert!("4".parse::<Version>().is_err());
        assert!("abc.def".parse::<Version>().is_err());
    }

    #[test]
    fn version_ordering() {
        assert!(Version::new(5, 0, 0) > Version::new(4, 2, 1));
        assert!(Version::new(4, 2, 1) > Version::new(4, 1, 9));
        assert!(Version::new(4, 2, 0) == Version::new(4, 2, 0));
    }

    #[test]
    fn version_display() {
        assert_eq!(Version::new(4, 2, 1).to_string(), "4.2.1");
    }
}
