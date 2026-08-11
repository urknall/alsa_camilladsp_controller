use crate::adapt::adapt_config;
use crate::alsa_listener::{AlsaLoopbackListener, DeviceListener};
use crate::args::Args;
use crate::camilla_ws::{
    parse_processing_state, parse_stop_reason, CamillaClient, CamillaWs, CommandReason,
    ProcessingState, StopReason, WsError,
};
use crate::error::{app_error, AppResult};
use crate::logging::{log, LogLevel};
use crate::wave::{DeviceSnapshot, WaveFormat};
use serde_json::Value as JsonValue;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Matches the Python listener's 50 ms debounce before reading ALSA controls.
const ALSA_DEBOUNCE_MS: u64 = 50;
/// Matches the Python controller's 200 ms event-queue poll interval.
const CONTROL_LOOP_MS: u32 = 200;
/// Maximum time (seconds) the controller waits for CamillaDSP to consume a
/// pending `SetConfig` before giving up and entering normal backoff/retry.
const PENDING_DEADLINE_SECS: u64 = 10;
/// Maximum time (seconds) CamillaDSP is allowed to remain in the `Starting`
/// state before the controller forces a restart.  In practice CamillaDSP
/// transitions through `Starting` in milliseconds; a multi-second hang
/// indicates a wedged process that will never recover on its own.
const STARTING_DEADLINE_SECS: u64 = 30;

// ─── Retry/backoff state ───────────────────────────────────────────────────

/// Exponential-backoff state for CamillaDSP restart attempts.
///
/// `latch_until_change` is set for permanent errors (e.g. config validation
/// failures) where retrying without a config change would be pointless.
/// It is cleared when the config file fingerprint changes.
struct RetryState {
    /// How many start attempts have been made since the last reset.
    consecutive: u32,
    /// Earliest time the next attempt may be made.
    next_at: Option<Instant>,
    /// Permanent failure latch — cleared by a config file change.
    latch_until_change: bool,
}

impl RetryState {
    fn new() -> Self {
        Self {
            consecutive: 0,
            next_at: None,
            latch_until_change: false,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    /// Returns `true` if enough time has elapsed since the last attempt and
    /// the permanent latch is not set.
    fn should_attempt(&self) -> bool {
        if self.latch_until_change {
            return false;
        }
        self.next_at.map(|t| Instant::now() >= t).unwrap_or(true)
    }

    /// Record a start attempt, setting the next backoff window.
    ///
    /// Backoff sequence: 500 ms → 1 s → 2 s → 5 s → 10 s → 30 s (cap).
    fn record_attempt(&mut self) {
        const DELAYS_MS: &[u64] = &[500, 1000, 2000, 5000, 10_000, 30_000];
        let delay = DELAYS_MS[self.consecutive.min(5) as usize];
        self.next_at = Some(Instant::now() + Duration::from_millis(delay));
        self.consecutive += 1;
    }

    /// Mark a permanent error; no further attempts until the latch clears.
    fn latch(&mut self) {
        self.latch_until_change = true;
    }
}

// ─── Config fingerprint ────────────────────────────────────────────────────

/// Lightweight fingerprint for detecting active config file changes without
/// polling the entire YAML content.
///
/// Tracks the canonicalized symlink target (catches CamillaGUI config
/// switches), the target file's mtime/size (catches in-place edits), and the
/// inode number (distinguishes files with identical size and visible mtime).
#[derive(Eq, PartialEq)]
struct ConfigFingerprint {
    target: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
    ino: u64,
}

impl ConfigFingerprint {
    #[allow(dead_code)]
    fn absent() -> Self {
        Self {
            target: PathBuf::new(),
            modified: None,
            size: 0,
            ino: 0,
        }
    }

    fn sample(path: &PathBuf) -> Self {
        let target = path.canonicalize().unwrap_or_else(|_| path.clone());
        let meta = fs::metadata(path);
        let modified = meta.as_ref().ok().and_then(|m| m.modified().ok());
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let ino = meta.as_ref().map(|m| m.ino()).unwrap_or(0);
        Self {
            target,
            modified,
            size,
            ino,
        }
    }
}

// ─── Controller ───────────────────────────────────────────────────────────

/// Ties together the ALSA listener and the CamillaDSP WebSocket client,
/// implementing the control loop that mirrors the Python reference controller's
/// `--adapt` behavior.
///
/// Generic over `D: DeviceListener` and `C: CamillaClient` to allow mock
/// injection for unit-testing the state machine.
pub struct Controller<D, C> {
    client: C,
    listener: D,
    adapt_path: PathBuf,
    fallback_wave: WaveFormat,
    /// The effective wave format used for the last (or pending) adaptation.
    current_wave: WaveFormat,
    /// Backoff/latch state for restart attempts.
    retry: RetryState,
    /// Timestamp of the last `SetConfig` that was accepted by CamillaDSP but
    /// not yet confirmed (i.e. CamillaDSP has not yet transitioned to
    /// Starting/Running/Paused/Stalled).  `None` means no pending start.
    ///
    /// Duplicate `SetConfig` calls are suppressed while a start is pending.
    /// If CamillaDSP remains `Inactive` with `StopReason::None` longer than
    /// [`PENDING_DEADLINE_SECS`], the pending state is cleared and the normal
    /// backoff/retry path takes over.
    pending_since: Option<Instant>,
    /// Fingerprint of the active config at the last check.
    config_fp: ConfigFingerprint,
    log_level: LogLevel,
}

impl<D: DeviceListener, C: CamillaClient> Controller<D, C> {
    // ── Internal helpers ────────────────────────────────────────────────

    fn stop_cdsp(&mut self) -> AppResult<()> {
        log(LogLevel::Info, self.log_level, "Stopping CamillaDSP");
        self.client.query("Stop", None)?;
        Ok(())
    }

    /// Re-read the active config file, adapt it to the current wave format,
    /// and send it to CamillaDSP via `SetConfig`.
    ///
    /// The config file is intentionally **re-read on every call** so that a
    /// CamillaGUI config switch is picked up the next time we try to start
    /// CamillaDSP, without relying on any cached state (fix for issue 1).
    ///
    /// If the retry backoff is active, the call is a no-op and returns `Ok`.
    fn start_cdsp(&mut self) -> AppResult<()> {
        if !self.retry.should_attempt() {
            log(
                LogLevel::Debug,
                self.log_level,
                "Retry backoff active, skipping start attempt",
            );
            return Ok(());
        }

        // Re-read and adapt the current active config file.
        let config = match adapt_config(&self.adapt_path, &self.current_wave) {
            Ok(c) => c,
            Err(err) => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Config adaptation failed, latching until file changes: {err}"),
                );
                self.retry.latch();
                return Ok(());
            }
        };

        // Record the attempt before sending; this sets the next backoff window.
        self.retry.record_attempt();

        log(
            LogLevel::Info,
            self.log_level,
            "Starting CamillaDSP with current config",
        );

        match self
            .client
            .query("SetConfig", Some(JsonValue::String(config)))
        {
            Ok(_) => {
                // SetConfig accepted.  CamillaDSP processes the config
                // asynchronously, so record the pending timestamp to prevent a
                // duplicate start before the state transition is confirmed.
                self.pending_since = Some(Instant::now());
                Ok(())
            }
            Err(WsError::Command(CommandReason::ConfigValidation, msg)) => {
                log(
                    LogLevel::Error,
                    self.log_level,
                    format!("Config validation error (latching until file changes): {msg}"),
                );
                self.retry.latch();
                Ok(())
            }
            Err(WsError::Command(CommandReason::RateLimit, msg)) => {
                // Transient — backoff already set by record_attempt().
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("CamillaDSP rate limit exceeded: {msg}"),
                );
                Ok(())
            }
            Err(WsError::Command(CommandReason::Shutdown, msg)) => {
                // Propagate as a transport error so the process exits and the
                // boot supervisor restarts it cleanly.
                Err(Box::new(WsError::Transport(format!(
                    "CamillaDSP is shutting down: {msg}"
                ))))
            }
            Err(WsError::Command(CommandReason::InvalidValue, msg)) => {
                // Programmer/protocol error — retrying will not help.
                log(
                    LogLevel::Error,
                    self.log_level,
                    format!("SetConfig InvalidValue error (latching until file changes): {msg}"),
                );
                self.retry.latch();
                Ok(())
            }
            Err(WsError::Command(CommandReason::Other, msg)) => {
                // Unknown command error — latch to avoid endless retry on a
                // permanent protocol-level failure.
                log(
                    LogLevel::Error,
                    self.log_level,
                    format!("SetConfig unknown command error (latching until file changes): {msg}"),
                );
                self.retry.latch();
                Ok(())
            }
            Err(err) => Err(Box::new(err)),
        }
    }

    fn handle_started(&mut self, snapshot: &DeviceSnapshot) -> AppResult<()> {
        self.current_wave = snapshot.wave.with_fallback(&self.fallback_wave);
        log(
            LogLevel::Info,
            self.log_level,
            format!("Device started with wave format {}", self.current_wave),
        );
        // Fresh ALSA start: clear retry/pending state and apply current config.
        self.retry.reset();
        self.pending_since = None;
        self.stop_cdsp()?;
        self.start_cdsp()
    }

    fn handle_stop_reason(
        &mut self,
        reason: StopReason,
        snapshot: &DeviceSnapshot,
    ) -> AppResult<()> {
        match reason {
            StopReason::CaptureFormatChange(reported_rate) => {
                log(
                    LogLevel::Info,
                    self.log_level,
                    format!(
                        "CamillaDSP stopped because capture format changed \
                         (reported rate {reported_rate})"
                    ),
                );
                // Re-read the loopback snapshot for a fresh format/channels.
                let current = self.listener.read_snapshot()?;
                let mut effective = current.wave.with_fallback(&self.fallback_wave);
                if effective.sample_rate.unwrap_or(0) == 0 && reported_rate > 0 {
                    effective.sample_rate = Some(reported_rate);
                }
                if effective.sample_rate.unwrap_or(0) > 0 {
                    self.current_wave = effective;
                    self.retry.reset();
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
                log(
                    LogLevel::Debug,
                    self.log_level,
                    "Capture is done, no action",
                );
            }
            StopReason::None => {
                log(LogLevel::Debug, self.log_level, "Initial/inactive state");
                if snapshot.active {
                    self.start_cdsp()?;
                }
            }
            StopReason::CaptureError(message) if snapshot.active => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Stopped due to capture error, scheduling retry: {message}"),
                );
                self.start_cdsp()?;
            }
            StopReason::PlaybackError(message) if snapshot.active => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Stopped due to playback error, scheduling retry: {message}"),
                );
                self.start_cdsp()?;
            }
            StopReason::CaptureError(_) | StopReason::PlaybackError(_) => {
                log(
                    LogLevel::Info,
                    self.log_level,
                    "Source is not active, not restarting after error",
                );
            }
            StopReason::UnknownError(message) if snapshot.active => {
                // Treat as a restartable fault (improvement over Python which
                // did not recognise this variant at all; fix for issue 7).
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Stopped with unknown error, scheduling retry: {message}"),
                );
                self.start_cdsp()?;
            }
            StopReason::UnknownError(message) => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Stopped with unknown error (source inactive): {message}"),
                );
            }
            StopReason::PlaybackFormatChange(rate) if snapshot.active => {
                // Treat as a restartable fault (fix for issue 7).
                log(
                    LogLevel::Info,
                    self.log_level,
                    format!("Playback format changed (rate {rate}), scheduling restart"),
                );
                self.start_cdsp()?;
            }
            StopReason::PlaybackFormatChange(rate) => {
                log(
                    LogLevel::Info,
                    self.log_level,
                    format!("Playback format changed (rate {rate}), source inactive"),
                );
            }
            StopReason::Unknown(value) => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Unknown CamillaDSP stop reason: {value}"),
                );
            }
        }
        Ok(())
    }

    /// Handle a single iteration of the `ProcessingState::Starting` branch.
    ///
    /// Arms (or re-arms) `pending_since` and forces a restart when CamillaDSP
    /// has been stuck in `Starting` longer than [`STARTING_DEADLINE_SECS`].
    /// Returns `Ok(())` in the normal case (still within deadline).
    fn check_starting_deadline(&mut self) -> AppResult<()> {
        // SetConfig was consumed; start (or re-arm) the timer.
        let since = *self.pending_since.get_or_insert_with(Instant::now);

        // Guard against a CamillaDSP process stuck indefinitely in
        // Starting (not a normal occurrence, but a defensive bound).
        if since.elapsed() >= Duration::from_secs(STARTING_DEADLINE_SECS) {
            log(
                LogLevel::Warning,
                self.log_level,
                format!(
                    "CamillaDSP stuck in Starting for >{STARTING_DEADLINE_SECS} s \
                     — forcing restart"
                ),
            );
            self.pending_since = None;
            self.stop_cdsp()?;
            self.start_cdsp()?;
        }
        Ok(())
    }

    fn process_inactive_state(&mut self, snapshot: &DeviceSnapshot) -> AppResult<()> {
        let reason = parse_stop_reason(self.client.query("GetStopReason", None)?)?;
        if let Some(since) = self.pending_since {
            // We sent SetConfig and are waiting for CamillaDSP to process it.
            // StopReason None means it hasn't consumed the config yet.
            if matches!(reason, StopReason::None) {
                if since.elapsed() < Duration::from_secs(PENDING_DEADLINE_SECS) {
                    log(
                        LogLevel::Debug,
                        self.log_level,
                        "Waiting for pending SetConfig to be applied",
                    );
                    return Ok(());
                }
                // Deadline elapsed — treat as a failed start and fall through
                // to normal backoff/retry so the controller does not wait forever.
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!(
                        "Pending SetConfig not consumed after {PENDING_DEADLINE_SECS} s \
                         — treating as failed start"
                    ),
                );
            }
            // Any real stop reason, or a timed-out pending: clear and retry normally.
            self.pending_since = None;
        }
        self.handle_stop_reason(reason, snapshot)
    }

    /// Run the main control loop until an unrecoverable error occurs.
    ///
    /// Loop structure (matches the Python reference):
    /// 1. Check config file fingerprint; clear error latch if it changed.
    /// 2. Wait on the HCTL file descriptor for up to `CONTROL_LOOP_MS`.
    /// 3. If an event fired: sleep 50 ms to debounce, drain kernel event buffer.
    /// 4. Read a fresh snapshot unconditionally.
    /// 5. Handle active/inactive transitions and wave-format changes.
    /// 6. Query CamillaDSP state; handle `Inactive` if not start-pending.
    pub fn run(mut self, mut previous: DeviceSnapshot) -> AppResult<()> {
        log(
            LogLevel::Info,
            self.log_level,
            "Starting ALSA loopback controller",
        );
        loop {
            // ── Config fingerprint check (issue 10) ──────────────────────
            let fp = ConfigFingerprint::sample(&self.adapt_path);
            if fp != self.config_fp {
                if self.retry.latch_until_change {
                    log(
                        LogLevel::Info,
                        self.log_level,
                        "Config file changed — clearing error latch",
                    );
                    self.retry.reset();
                }
                self.config_fp = fp;
            }

            // ── ALSA event wait ──────────────────────────────────────────
            if self.listener.wait_for_event(CONTROL_LOOP_MS)? {
                thread::sleep(Duration::from_millis(ALSA_DEBOUNCE_MS));
                self.listener.handle_events()?;
            }

            let current = self.listener.read_snapshot()?;

            // ── ALSA state transitions ───────────────────────────────────
            if !previous.active && current.active {
                self.handle_started(&current)?;
            } else if previous.active && !current.active {
                log(LogLevel::Info, self.log_level, "Device stopped");
                self.stop_cdsp()?;
                // Reset retry so the next start attempt is immediate.
                self.retry.reset();
                self.pending_since = None;
            } else if previous.active && current.active && previous.wave != current.wave {
                // Mirrors the Python listener's STOPPED-then-STARTED pair.
                log(
                    LogLevel::Info,
                    self.log_level,
                    format!("Device wave format changed to {}", current.wave),
                );
                self.stop_cdsp()?;
                self.current_wave = current.wave.with_fallback(&self.fallback_wave);
                self.retry.reset();
                self.pending_since = None;
                self.start_cdsp()?;
            }

            previous = current.clone();

            // ── CamillaDSP state ─────────────────────────────────────────
            let state = parse_processing_state(self.client.query("GetState", None)?)?;
            match state {
                ProcessingState::Running | ProcessingState::Paused | ProcessingState::Stalled => {
                    // Confirmed success — reset retry and pending.
                    if self.pending_since.is_some() || self.retry.consecutive > 0 {
                        self.retry.reset();
                    }
                    self.pending_since = None;
                }
                ProcessingState::Starting => {
                    self.check_starting_deadline()?;
                }
                ProcessingState::Inactive => {
                    self.process_inactive_state(&current)?;
                }
                ProcessingState::Unknown(value) => {
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        format!("Unknown CamillaDSP processing state: {value}"),
                    );
                }
            }
        }
    }
}

/// Concrete constructor using the production ALSA listener and WebSocket client.
impl Controller<AlsaLoopbackListener, CamillaWs> {
    pub fn new(args: &Args) -> AppResult<(Self, DeviceSnapshot)> {
        let listener = AlsaLoopbackListener::new(&args.device, args.log_level)?;
        let initial = listener.read_snapshot()?;
        let client = CamillaWs::connect(&args.host, args.port)?;

        let fallback_wave = WaveFormat {
            sample_rate: args.initial_rate,
            sample_format: args.initial_format.clone(),
            channels: args.initial_channels,
        };
        let adapt_path = args
            .adapt
            .clone()
            .ok_or_else(|| app_error("--adapt is required in controller mode"))?;

        // Set current_wave from the initial snapshot so start_cdsp uses the
        // correct format even on the very first GetState → Inactive path.
        let current_wave = initial.wave.with_fallback(&fallback_wave);
        let config_fp = ConfigFingerprint::sample(&adapt_path);

        let controller = Self {
            client,
            listener,
            adapt_path,
            fallback_wave,
            current_wave,
            retry: RetryState::new(),
            pending_since: None,
            config_fp,
            log_level: args.log_level,
        };
        Ok((controller, initial))
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alsa_listener::DeviceListener;
    use crate::camilla_ws::{CamillaClient, CommandReason, WsError};
    use crate::error::AppResult;
    use crate::logging::LogLevel;
    use crate::wave::{DeviceSnapshot, WaveFormat};
    use serde_json::Value as JsonValue;
    use std::collections::VecDeque;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    // ── Mock infrastructure ─────────────────────────────────────────────

    /// Records every SetConfig payload and returns pre-scripted responses.
    struct MockClient {
        responses: VecDeque<Result<Option<JsonValue>, WsError>>,
        sent_configs: Vec<String>,
    }

    impl MockClient {
        fn new(responses: Vec<Result<Option<JsonValue>, WsError>>) -> Self {
            Self {
                responses: responses.into(),
                sent_configs: Vec::new(),
            }
        }
        fn ok() -> Result<Option<JsonValue>, WsError> {
            Ok(None)
        }
        #[allow(dead_code)]
        fn state(s: &str) -> Result<Option<JsonValue>, WsError> {
            Ok(Some(JsonValue::String(s.to_owned())))
        }
        fn stop_reason(r: &str) -> Result<Option<JsonValue>, WsError> {
            Ok(Some(JsonValue::String(r.to_owned())))
        }
    }

    impl CamillaClient for MockClient {
        fn query(
            &mut self,
            command: &str,
            argument: Option<JsonValue>,
        ) -> Result<Option<JsonValue>, WsError> {
            if command == "SetConfig" {
                if let Some(JsonValue::String(ref cfg)) = argument {
                    self.sent_configs.push(cfg.clone());
                }
            }
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err(WsError::Transport("no more responses".to_owned())))
        }
    }

    struct MockListener {
        snapshots: VecDeque<DeviceSnapshot>,
    }

    impl MockListener {
        fn new(snapshots: Vec<DeviceSnapshot>) -> Self {
            Self {
                snapshots: snapshots.into(),
            }
        }
        fn active_with_rate(rate: u32) -> DeviceSnapshot {
            DeviceSnapshot {
                active: true,
                wave: WaveFormat {
                    sample_rate: Some(rate),
                    sample_format: Some("S32_LE".to_owned()),
                    channels: Some(2),
                },
            }
        }
        fn inactive() -> DeviceSnapshot {
            DeviceSnapshot {
                active: false,
                wave: WaveFormat::default(),
            }
        }
    }

    impl DeviceListener for MockListener {
        fn wait_for_event(&self, _timeout_ms: u32) -> AppResult<bool> {
            Ok(false)
        }
        fn handle_events(&self) -> AppResult<()> {
            Ok(())
        }
        fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
            self.snapshots
                .front()
                .cloned()
                .ok_or_else(|| app_error("MockListener: no more snapshots"))
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "picoredsp-ctrl-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn minimal_config(device: &str) -> String {
        format!(
            "devices:\n  samplerate: 44100\n  chunksize: 2048\n  \
             capture:\n    type: Alsa\n    channels: 2\n    \
             device: \"hw:Loopback,0,0\"\n  \
             playback:\n    type: Alsa\n    channels: 2\n    \
             device: \"{device}\"\n\
             filters: {{}}\nmixers: {{}}\npipeline: []\nprocessors: {{}}\n"
        )
    }

    fn make_controller(
        client: MockClient,
        listener: MockListener,
        adapt_path: PathBuf,
    ) -> Controller<MockListener, MockClient> {
        Controller {
            client,
            listener,
            current_wave: WaveFormat {
                sample_rate: Some(44100),
                sample_format: Some("S32_LE".to_owned()),
                channels: Some(2),
            },
            fallback_wave: WaveFormat::default(),
            adapt_path: adapt_path.clone(),
            retry: RetryState::new(),
            pending_since: None,
            config_fp: ConfigFingerprint::absent(),
            log_level: LogLevel::Error,
        }
    }

    // ── Tests ───────────────────────────────────────────────────────────

    /// Issue 1: start_cdsp() must always re-read the active config file.
    /// Retargeting the symlink (as CamillaGUI does) must be reflected in the
    /// next restart, even when the wave format has not changed.
    #[test]
    fn start_cdsp_follows_symlink_retarget() {
        let dir = test_dir("symlink-retarget");
        let config_a = dir.join("RoomCorrection.yml");
        let config_b = dir.join("Headphones.yml");
        let active = dir.join("active_config.yml");

        fs::write(&config_a, minimal_config("hw:CardA,0")).unwrap();
        fs::write(&config_b, minimal_config("hw:CardB,0")).unwrap();
        symlink(&config_a, &active).unwrap();

        let mut ctrl = make_controller(
            MockClient::new(vec![MockClient::ok(), MockClient::ok()]),
            MockListener::new(vec![]),
            active.clone(),
        );

        // First start → config_a should be sent.
        ctrl.start_cdsp().unwrap();
        assert_eq!(ctrl.client.sent_configs.len(), 1);
        assert!(
            ctrl.client.sent_configs[0].contains("hw:CardA,0"),
            "first start should use CardA"
        );

        // Retarget symlink to config_b, reset retry.
        ctrl.retry.reset();
        ctrl.pending_since = None;
        fs::remove_file(&active).unwrap();
        symlink(&config_b, &active).unwrap();

        // Second start → config_b must be sent, not the cached config_a.
        ctrl.start_cdsp().unwrap();
        assert_eq!(ctrl.client.sent_configs.len(), 2);
        assert!(
            ctrl.client.sent_configs[1].contains("hw:CardB,0"),
            "second start should use CardB"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 5 & 6: After SetConfig succeeds (pending=true), a subsequent
    /// Inactive + StopReason=None must NOT issue a second SetConfig.
    #[test]
    fn start_pending_suppresses_duplicate_set_config() {
        let dir = test_dir("start-pending");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::ok(),                // SetConfig OK
            MockClient::stop_reason("None"), // GetStopReason → None (still pending)
        ]);
        let listener = MockListener::new(vec![
            MockListener::active_with_rate(44100), // read_snapshot for process_inactive
        ]);
        let mut ctrl = make_controller(client, listener, active.clone());

        // Simulate: start_cdsp() called normally.
        ctrl.start_cdsp().unwrap();
        assert!(
            ctrl.pending_since.is_some(),
            "should be pending after successful SetConfig"
        );
        assert_eq!(ctrl.client.sent_configs.len(), 1);

        // Simulate: process_inactive_state called while still pending.
        let snap = MockListener::active_with_rate(44100);
        ctrl.process_inactive_state(&snap).unwrap();

        // No additional SetConfig should have been sent.
        assert_eq!(
            ctrl.client.sent_configs.len(),
            1,
            "no duplicate SetConfig while start_pending"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 5: PlaybackError while source is inactive must NOT trigger a restart.
    #[test]
    fn playback_error_with_inactive_source_skips_restart() {
        let dir = test_dir("pb-error-inactive");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![]); // no responses expected
        let listener = MockListener::new(vec![]);
        let mut ctrl = make_controller(client, listener, active.clone());

        let inactive_snap = MockListener::inactive();
        ctrl.handle_stop_reason(StopReason::PlaybackError("XRUN".to_owned()), &inactive_snap)
            .unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "no SetConfig with inactive source"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 5: Exponential backoff limits restart rate after repeated failures.
    #[test]
    fn retry_backoff_prevents_immediate_reattempt() {
        let dir = test_dir("backoff");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let mut ctrl = make_controller(
            MockClient::new(vec![MockClient::ok(), MockClient::ok()]),
            MockListener::new(vec![]),
            active.clone(),
        );

        // First attempt succeeds (backoff sets next_at).
        ctrl.start_cdsp().unwrap();
        assert_eq!(ctrl.client.sent_configs.len(), 1);

        // Second call immediately: backoff should suppress it.
        ctrl.pending_since = None; // simulate start failed asynchronously
        ctrl.start_cdsp().unwrap();
        assert_eq!(
            ctrl.client.sent_configs.len(),
            1,
            "backoff should suppress immediate second attempt"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 9: ConfigValidationError latches until the config file changes.
    #[test]
    fn config_validation_error_latches_retry() {
        let dir = test_dir("validation-latch");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            // SetConfig returns ConfigValidationError.
            Err(WsError::Command(
                CommandReason::ConfigValidation,
                "bad filter".to_owned(),
            )),
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        ctrl.start_cdsp().unwrap();
        assert!(
            ctrl.retry.latch_until_change,
            "should be latched after validation error"
        );

        // A second call must not produce another SetConfig.
        ctrl.start_cdsp().unwrap();
        assert_eq!(
            ctrl.client.sent_configs.len(),
            1,
            "latched — no second SetConfig"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 7: UnknownError with active source triggers a restart attempt.
    #[test]
    fn unknown_error_with_active_source_schedules_retry() {
        let dir = test_dir("unknown-err");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![MockClient::ok()]);
        let listener = MockListener::new(vec![]);
        let mut ctrl = make_controller(client, listener, active.clone());

        let active_snap = MockListener::active_with_rate(44100);
        ctrl.handle_stop_reason(StopReason::UnknownError("oom".to_owned()), &active_snap)
            .unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            1,
            "should have attempted a restart"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 6: 30-second Starting-state timeout forces a restart.
    ///
    /// Simulates CamillaDSP stuck in `Starting` by setting `pending_since` to
    /// a timestamp 31 seconds in the past, then calling
    /// `check_starting_deadline`.  The controller must issue `Stop` and a new
    /// `SetConfig`.
    #[test]
    fn starting_timeout_forces_restart() {
        let dir = test_dir("starting-timeout");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::ok(), // Stop
            MockClient::ok(), // SetConfig (restart attempt)
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        // Pre-arm pending_since with a timestamp 31 seconds in the past to
        // simulate CamillaDSP being stuck in Starting beyond the deadline.
        ctrl.pending_since = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(STARTING_DEADLINE_SECS + 1))
                .unwrap(),
        );

        ctrl.check_starting_deadline().unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            1,
            "should have sent a new SetConfig after Starting timeout"
        );
        assert!(
            ctrl.pending_since.is_some(),
            "pending_since re-armed by the fresh SetConfig"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 6: Within the Starting deadline, `check_starting_deadline` is a no-op.
    #[test]
    fn starting_within_deadline_does_nothing() {
        let dir = test_dir("starting-ok");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![]); // no responses expected
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        // pending_since is None (will be set to now() by check_starting_deadline).
        ctrl.check_starting_deadline().unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "no restart within deadline"
        );
        assert!(
            ctrl.pending_since.is_some(),
            "pending_since was initialised"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 6: Pending-SetConfig deadline expiry triggers a retry.
    ///
    /// When `pending_since` is set but CamillaDSP stays `Inactive` with
    /// `StopReason::None` for longer than `PENDING_DEADLINE_SECS`, the
    /// controller must clear the pending state and fall through to the normal
    /// retry path (issuing a new `SetConfig`).
    #[test]
    fn pending_deadline_expired_triggers_retry() {
        let dir = test_dir("pending-deadline");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::stop_reason("None"), // GetStopReason — still None after deadline
            MockClient::ok(),                // SetConfig (retry after deadline clears)
        ]);
        let listener = MockListener::new(vec![MockListener::active_with_rate(44100)]);
        let mut ctrl = make_controller(client, listener, active.clone());

        // Simulate a pending_since that is already past the deadline.
        ctrl.pending_since = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(PENDING_DEADLINE_SECS + 1))
                .unwrap(),
        );

        let snap = MockListener::active_with_rate(44100);
        ctrl.process_inactive_state(&snap).unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            1,
            "should have retried after pending deadline"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 7: PlaybackFormatChange with active source schedules a restart.
    #[test]
    fn playback_format_change_with_active_source_restarts() {
        let dir = test_dir("pb-fmt-change");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![MockClient::ok()]); // SetConfig
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        let active_snap = MockListener::active_with_rate(96000);
        ctrl.handle_stop_reason(StopReason::PlaybackFormatChange(96000), &active_snap)
            .unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            1,
            "should restart on PlaybackFormatChange with active source"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Issue 7: PlaybackFormatChange with inactive source must NOT restart.
    #[test]
    fn playback_format_change_with_inactive_source_skips_restart() {
        let dir = test_dir("pb-fmt-change-inactive");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![]); // no responses expected
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        let inactive_snap = MockListener::inactive();
        ctrl.handle_stop_reason(StopReason::PlaybackFormatChange(96000), &inactive_snap)
            .unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "no restart when source is inactive"
        );

        fs::remove_dir_all(dir).unwrap();
    }
}
