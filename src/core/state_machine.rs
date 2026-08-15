use crate::backend::{ControllerBackend, StreamEvent};
use crate::camilladsp::websocket::{
    parse_processing_state, parse_stop_reason, CamillaClient, CommandReason, ProcessingState,
    StopReason, WsError,
};
use crate::core::adaptation::adapt_config;
use crate::core::config::{DeviceSnapshot, WaveFormat};
use crate::core::errors::AppResult;
use crate::core::logging::{log, LogLevel};
use crate::core::recovery::{ConfigFingerprint, RetryState};
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
/// After sending an idle `Stop` (source inactive, CamillaDSP still running),
/// wait at least this many seconds before re-sending the stop.  This guards
/// against a GUI `SetConfig` racing with an in-flight stop and leaving
/// CamillaDSP permanently running while the source stays inactive, without
/// causing Stop flooding every 200 ms.
const IDLE_STOP_RETRY_SECS: u64 = 2;

// ─── Controller ───────────────────────────────────────────────────────────

/// Ties together the ALSA listener and the CamillaDSP WebSocket client,
/// implementing the control loop that mirrors the Python reference controller's
/// `--adapt` behavior.
///
/// Generic over `B: ControllerBackend` and `C: CamillaClient` to allow mock
/// injection for unit-testing the state machine.
pub struct Controller<B: ControllerBackend, C> {
    client: C,
    stream_backend: B,
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
    /// Timestamp of the last `Stop` sent to enforce the idle invariant (source
    /// inactive, CamillaDSP still running/starting).  `None` means no stop has
    /// been sent yet in the current idle period.
    ///
    /// A stop is re-sent after [`IDLE_STOP_RETRY_SECS`] seconds to recover
    /// from a GUI-triggered `SetConfig` that races with an in-flight stop.
    /// Cleared when CamillaDSP reaches `Inactive` or the source becomes active.
    idle_stop_since: Option<Instant>,
    /// Fingerprint of the active config at the last check.
    config_fp: ConfigFingerprint,
    log_level: LogLevel,
}

impl<B: ControllerBackend, C: CamillaClient> Controller<B, C> {
    /// Construct a controller with explicit backend/client wiring.
    pub fn new(
        client: C,
        stream_backend: B,
        adapt_path: PathBuf,
        fallback_wave: WaveFormat,
        current_wave: WaveFormat,
        log_level: LogLevel,
    ) -> Self {
        Self {
            client,
            stream_backend,
            config_fp: ConfigFingerprint::sample(&adapt_path),
            adapt_path,
            fallback_wave,
            current_wave,
            retry: RetryState::new(),
            pending_since: None,
            idle_stop_since: None,
            log_level,
        }
    }

    // ── Internal helpers ────────────────────────────────────────────────

    fn stop_cdsp(&mut self) -> AppResult<()> {
        log(LogLevel::Info, self.log_level, "Stopping CamillaDSP");
        self.client.query("Stop", None)?;
        Ok(())
    }

    /// Enforce the idle invariant: source is inactive but CamillaDSP is still
    /// running (e.g. started externally via CamillaGUI Apply while playback
    /// was stopped).
    ///
    /// Sends a `Stop` immediately on the first call in an idle period, then
    /// rate-limits re-sends to once every [`IDLE_STOP_RETRY_SECS`] seconds so
    /// that a `SetConfig` that races with the in-flight stop cannot permanently
    /// avert the invariant without causing Stop flooding on every loop tick.
    fn enforce_idle_invariant(&mut self) -> AppResult<()> {
        let should_stop = match self.idle_stop_since {
            None => true,
            Some(since) => since.elapsed() >= Duration::from_secs(IDLE_STOP_RETRY_SECS),
        };
        if should_stop {
            log(
                LogLevel::Info,
                self.log_level,
                "Source inactive but CamillaDSP is running — enforcing idle invariant (Stop)",
            );
            self.stop_cdsp()?;
            self.idle_stop_since = Some(Instant::now());
            self.pending_since = None;
        }
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
    fn start_cdsp_with_wave(&mut self, wave: &WaveFormat) -> AppResult<()> {
        if !self.retry.should_attempt() {
            log(
                LogLevel::Debug,
                self.log_level,
                "Retry backoff active, skipping start attempt",
            );
            return Ok(());
        }

        // Re-read and adapt the current active config file.
        let config = match adapt_config(&self.adapt_path, wave) {
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

    fn start_cdsp(&mut self) -> AppResult<()> {
        let wave = self.current_wave.clone();
        self.start_cdsp_with_wave(&wave)
    }

    fn handle_started(&mut self, snapshot: &DeviceSnapshot) -> AppResult<()> {
        self.current_wave = snapshot.wave.with_fallback(&self.fallback_wave);
        log(
            LogLevel::Info,
            self.log_level,
            format!("Device started with wave format {}", self.current_wave),
        );
        // Fresh ALSA start: clear retry/pending/idle state and apply current config.
        self.retry.reset();
        self.pending_since = None;
        self.idle_stop_since = None;
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
                let current = self.stream_backend.read_snapshot()?;
                if !current.active {
                    log(
                        LogLevel::Info,
                        self.log_level,
                        "Capture format changed, but source is no longer active; waiting for playback",
                    );
                    self.retry.reset();
                    self.pending_since = None;
                    return Ok(());
                }
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

    /// Perform a one-time bootstrap on controller startup.
    ///
    /// When CamillaDSP starts with `--wait --no_config`, the processing state
    /// is `Inactive` until a config is loaded.
    ///
    /// Bootstrap behavior is split by source state:
    /// * active=true  → adapt with live ALSA wave format before SetConfig.
    /// * active=false → do not send SetConfig; wait for first active playback
    ///   stream so snd-aloop capture is not opened before the player.
    fn bootstrap_initial_config(&mut self, snapshot: &DeviceSnapshot) -> AppResult<()> {
        let state = parse_processing_state(self.client.query("GetState", None)?)?;
        match state {
            ProcessingState::Inactive => {
                self.retry.reset();
                self.pending_since = None;
                if snapshot.active {
                    self.current_wave = snapshot.wave.with_fallback(&self.fallback_wave);
                    log(
                        LogLevel::Info,
                        self.log_level,
                        format!(
                            "Bootstrapping initial config for active source ({})",
                            self.current_wave
                        ),
                    );
                    self.start_cdsp()?;
                } else {
                    log(
                        LogLevel::Info,
                        self.log_level,
                        "Source inactive at startup — waiting for playback before loading CamillaDSP config",
                    );
                }
            }
            ProcessingState::Running | ProcessingState::Paused | ProcessingState::Stalled => {
                if !snapshot.active {
                    self.enforce_idle_invariant()?;
                } else {
                    log(
                        LogLevel::Debug,
                        self.log_level,
                        "Skipping bootstrap because CamillaDSP is already active",
                    );
                }
            }
            ProcessingState::Starting => {
                if !snapshot.active {
                    self.enforce_idle_invariant()?;
                } else {
                    log(
                        LogLevel::Debug,
                        self.log_level,
                        "Skipping bootstrap while CamillaDSP is already starting",
                    );
                }
            }
            ProcessingState::Unknown(value) => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Unknown CamillaDSP processing state during bootstrap: {value}"),
                );
            }
        }
        Ok(())
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
    pub fn run(mut self, initial: DeviceSnapshot) -> AppResult<()> {
        log(
            LogLevel::Info,
            self.log_level,
            "Starting ALSA loopback controller",
        );
        self.bootstrap_initial_config(&initial)?;
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
                }
                // Any deliberate config change is a good reason to allow an
                // immediate new start attempt even if we are in a normal backoff
                // window (e.g. after a hardware error or rate-limit).
                self.retry.reset();
                self.config_fp = fp;
            }

            // ── ALSA stream event detection ──────────────────────────────
            let stream_event = self.stream_backend.poll_event(CONTROL_LOOP_MS)?;
            let current = self.stream_backend.current_snapshot().clone();
            if let Some(event) = stream_event {
                match event {
                    StreamEvent::Started(_) => {
                        self.handle_started(&current)?;
                    }
                    StreamEvent::Stopped => {
                        log(LogLevel::Info, self.log_level, "Device stopped");
                        self.stop_cdsp()?;
                        self.idle_stop_since = Some(Instant::now());
                        // Reset retry so the next start attempt is immediate.
                        self.retry.reset();
                        self.pending_since = None;
                    }
                    StreamEvent::Changed(_) => {
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
                }
            }

            // ── CamillaDSP state ─────────────────────────────────────────
            let state = parse_processing_state(self.client.query("GetState", None)?)?;
            self.handle_processing_state(state, &current)?;
        }
    }

    /// Dispatch a CamillaDSP processing state within the control loop.
    ///
    /// Extracted for unit-testability: tests can drive this directly without
    /// running the infinite `run()` loop.
    fn handle_processing_state(
        &mut self,
        state: ProcessingState,
        current: &DeviceSnapshot,
    ) -> AppResult<()> {
        match state {
            ProcessingState::Running | ProcessingState::Paused | ProcessingState::Stalled => {
                if !current.active {
                    self.enforce_idle_invariant()?;
                } else {
                    // Confirmed success — reset retry and pending.
                    if self.pending_since.is_some() || self.retry.consecutive > 0 {
                        self.retry.reset();
                    }
                    self.pending_since = None;
                }
            }
            ProcessingState::Starting => {
                if !current.active {
                    self.enforce_idle_invariant()?;
                } else {
                    self.check_starting_deadline()?;
                }
                // If !current.active: enforce_idle_invariant handles the stop.
                // Do not invoke check_starting_deadline when inactive because that
                // helper would eventually call start_cdsp(), which must not happen
                // while the source is inactive.
            }
            ProcessingState::Inactive => {
                self.idle_stop_since = None;
                self.process_inactive_state(current)?;
            }
            ProcessingState::Unknown(value) => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("Unknown CamillaDSP processing state: {value}"),
                );
            }
        }
        Ok(())
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camilladsp::websocket::{CamillaClient, CommandReason, WsError};
    use crate::core::config::{DeviceSnapshot, WaveFormat};
    use crate::core::errors::AppResult;
    use crate::core::logging::LogLevel;
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
        /// Number of times a Stop command was issued.
        stop_count: u32,
    }

    impl MockClient {
        fn new(responses: Vec<Result<Option<JsonValue>, WsError>>) -> Self {
            Self {
                responses: responses.into(),
                sent_configs: Vec::new(),
                stop_count: 0,
            }
        }
        fn ok() -> Result<Option<JsonValue>, WsError> {
            Ok(None)
        }
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
            if command == "Stop" {
                self.stop_count += 1;
            }
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err(WsError::Transport("no more responses".to_owned())))
        }
    }

    /// Mock stream backend that directly implements `ControllerBackend`.
    ///
    /// `poll_event` always returns `None`; the tests exercise the state machine
    /// handlers directly rather than through the run loop.  `read_snapshot`
    /// peeks at the front of the pre-loaded snapshot queue (without consuming
    /// it) to supply the fresh snapshot used by `handle_stop_reason` for
    /// `CaptureFormatChange`.
    struct MockListener {
        snapshots: VecDeque<DeviceSnapshot>,
        current: DeviceSnapshot,
    }

    impl MockListener {
        fn new(snapshots: Vec<DeviceSnapshot>) -> Self {
            Self {
                snapshots: snapshots.into(),
                current: MockListener::inactive(),
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

    impl ControllerBackend for MockListener {
        fn poll_event(&mut self, _timeout_ms: u32) -> AppResult<Option<StreamEvent>> {
            Ok(None)
        }
        fn current_snapshot(&self) -> &DeviceSnapshot {
            &self.current
        }
        fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
            self.snapshots
                .front()
                .cloned()
                .ok_or_else(|| crate::core::errors::app_error("MockListener: no more snapshots"))
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
        let fallback_wave = WaveFormat::default();
        Controller {
            client,
            stream_backend: listener,
            current_wave: WaveFormat {
                sample_rate: Some(44100),
                sample_format: Some("S32_LE".to_owned()),
                channels: Some(2),
            },
            fallback_wave,
            adapt_path: adapt_path.clone(),
            retry: RetryState::new(),
            pending_since: None,
            idle_stop_since: None,
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

    /// Gate 0 acceptance: Apply and Save updates to the active config file are
    /// re-read on the next restart even when the active symlink target and wave
    /// format stay the same.
    #[test]
    fn acceptance_gui_apply_and_save_rereads_saved_active_file() {
        let dir = test_dir("gui-apply-save");
        let config = dir.join("RoomCorrection.yml");
        let active = dir.join("active_config.yml");

        fs::write(&config, minimal_config("hw:CardA,0")).unwrap();
        symlink(&config, &active).unwrap();

        let mut ctrl = make_controller(
            MockClient::new(vec![MockClient::ok(), MockClient::ok()]),
            MockListener::new(vec![]),
            active.clone(),
        );

        ctrl.start_cdsp().unwrap();
        assert_eq!(ctrl.client.sent_configs.len(), 1);
        assert!(
            ctrl.client.sent_configs[0].contains("hw:CardA,0"),
            "first start should use the initial saved config"
        );

        ctrl.retry.reset();
        ctrl.pending_since = None;
        fs::write(&config, minimal_config("hw:CardB,0")).unwrap();

        ctrl.start_cdsp().unwrap();
        assert_eq!(ctrl.client.sent_configs.len(), 2);
        assert!(
            ctrl.client.sent_configs[1].contains("hw:CardB,0"),
            "second start should re-read the saved file contents"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Gate 0 acceptance: persisted active-config file contents survive a
    /// controller restart/reboot and are only applied after playback becomes
    /// active.
    #[test]
    fn acceptance_reboot_persistence_applies_saved_config_after_restart() {
        let dir = test_dir("acceptance-reboot-persistence");
        let config = dir.join("RoomCorrection.yml");
        let active = dir.join("active_config.yml");

        fs::write(&config, minimal_config("hw:CardA,0")).unwrap();
        symlink(&config, &active).unwrap();

        let mut before_restart = make_controller(
            MockClient::new(vec![MockClient::ok(), MockClient::ok()]),
            MockListener::new(vec![]),
            active.clone(),
        );
        before_restart
            .handle_started(&MockListener::active_with_rate(44100))
            .unwrap();
        assert_eq!(before_restart.client.sent_configs.len(), 1);
        assert!(
            before_restart.client.sent_configs[0].contains("hw:CardA,0"),
            "first run should apply the pre-restart saved config"
        );

        fs::write(&config, minimal_config("hw:CardB,0")).unwrap();

        let mut after_restart = make_controller(
            MockClient::new(vec![
                MockClient::state("Inactive"), // bootstrap state on restart
                MockClient::ok(),              // Stop on first playback
                MockClient::ok(),              // SetConfig on first playback
            ]),
            MockListener::new(vec![]),
            active.clone(),
        );

        after_restart
            .bootstrap_initial_config(&MockListener::inactive())
            .unwrap();
        assert_eq!(
            after_restart.client.sent_configs.len(),
            0,
            "restart while source is inactive must remain idle"
        );

        after_restart
            .handle_started(&MockListener::active_with_rate(44100))
            .unwrap();
        assert_eq!(after_restart.client.sent_configs.len(), 1);
        assert!(
            after_restart.client.sent_configs[0].contains("hw:CardB,0"),
            "post-restart playback should use persisted saved config"
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

    /// Boot bootstrap: when CamillaDSP is Inactive and source is inactive, the
    /// controller must not load any config yet.
    #[test]
    fn bootstrap_does_not_open_capture_for_inactive_source() {
        let dir = test_dir("bootstrap-inactive");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![MockClient::state("Inactive")]); // GetState (bootstrap)
        let listener = MockListener::new(vec![]);
        let mut ctrl = make_controller(client, listener, active.clone());

        let inactive_snap = DeviceSnapshot {
            active: false,
            // Stale wave values from a previous stream must be ignored.
            wave: WaveFormat {
                sample_rate: Some(96000),
                sample_format: Some("S24_3_LE".to_owned()),
                channels: Some(6),
            },
        };
        ctrl.bootstrap_initial_config(&inactive_snap).unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "inactive startup must not SetConfig because CamillaDSP must not open the loopback before the player"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Boot bootstrap: when CamillaDSP is Inactive and source already runs, the
    /// first SetConfig must use the live ALSA format immediately.
    #[test]
    fn bootstrap_uses_live_wave_for_active_source() {
        let dir = test_dir("bootstrap-active");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::state("Inactive"), // GetState (bootstrap)
            MockClient::ok(),              // SetConfig
        ]);
        let listener = MockListener::new(vec![]);
        let mut ctrl = make_controller(client, listener, active.clone());

        let active_snap = MockListener::active_with_rate(96000);
        ctrl.bootstrap_initial_config(&active_snap).unwrap();

        assert_eq!(ctrl.client.sent_configs.len(), 1);
        assert!(
            ctrl.client.sent_configs[0].contains("samplerate: 96000"),
            "bootstrap must adapt to the running source sample rate"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// First-playback path: startup inactive must not configure CamillaDSP
    /// until the source becomes active, then SetConfig must use live ALSA rate.
    #[test]
    fn inactive_startup_then_first_playback_applies_live_wave() {
        let dir = test_dir("bootstrap-inactive-then-active");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::state("Inactive"), // GetState (bootstrap)
            MockClient::ok(),              // Stop
            MockClient::ok(),              // SetConfig
        ]);
        let listener = MockListener::new(vec![]);
        let mut ctrl = make_controller(client, listener, active.clone());

        let inactive_snap = MockListener::inactive();
        ctrl.bootstrap_initial_config(&inactive_snap).unwrap();
        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "bootstrap must not load config while source is inactive"
        );

        let active_snap = DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: Some(48000),
                sample_format: Some("S16_LE".to_owned()),
                channels: Some(2),
            },
        };
        ctrl.handle_started(&active_snap).unwrap();

        assert_eq!(ctrl.client.sent_configs.len(), 1);
        assert!(
            ctrl.client.sent_configs[0].contains("samplerate: 48000"),
            "first playback must adapt to the live source sample rate"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn capture_format_change_with_inactive_fresh_snapshot_skips_restart() {
        let dir = test_dir("capture-fmt-change-inactive");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let stale_inactive_snapshot = DeviceSnapshot {
            active: false,
            wave: WaveFormat {
                sample_rate: Some(48000),
                sample_format: Some("S32_LE".to_owned()),
                channels: Some(2),
            },
        };
        let client = MockClient::new(vec![]);
        let listener = MockListener::new(vec![stale_inactive_snapshot.clone()]);
        let mut ctrl = make_controller(client, listener, active.clone());

        ctrl.handle_stop_reason(
            StopReason::CaptureFormatChange(48000),
            &stale_inactive_snapshot,
        )
        .unwrap();

        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "no SetConfig when the fresh snapshot is inactive"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn capture_format_change_with_active_snapshot_uses_reported_rate_fallback() {
        let dir = test_dir("capture-fmt-change-reported-rate");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let fresh_active_snapshot = DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: None,
                sample_format: Some("S32_LE".to_owned()),
                channels: Some(2),
            },
        };
        let client = MockClient::new(vec![
            MockClient::ok(), // Stop
            MockClient::ok(), // SetConfig
        ]);
        let listener = MockListener::new(vec![fresh_active_snapshot.clone()]);
        let mut ctrl = make_controller(client, listener, active.clone());

        ctrl.handle_stop_reason(
            StopReason::CaptureFormatChange(96000),
            &fresh_active_snapshot,
        )
        .unwrap();

        assert_eq!(ctrl.client.sent_configs.len(), 1);
        assert!(
            ctrl.client.sent_configs[0].contains("samplerate: 96000"),
            "reported capture rate should be used when the fresh snapshot rate is unknown"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// P2 idle invariant — first half:
    /// When the source is inactive but CamillaDSP is Running (e.g. started
    /// externally via CamillaGUI Apply), the controller must send exactly one
    /// Stop and must not send SetConfig.  Subsequent loop ticks while the
    /// source remains inactive must not re-send Stop.
    #[test]
    fn idle_invariant_stops_running_cdsp_once_when_source_inactive() {
        let dir = test_dir("idle-invariant-stop");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        // One Stop response for the first enforcement tick; the second tick
        // must not consume any response (flag already set).
        let client = MockClient::new(vec![
            MockClient::ok(), // Stop (first tick)
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());
        let inactive_snap = MockListener::inactive();

        // First tick: idle invariant fires → sends Stop.
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert!(
            ctrl.idle_stop_since.is_some(),
            "guard must be set after first Stop"
        );
        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "no SetConfig must be sent during idle enforcement"
        );

        // Second tick: flag is set → no additional Stop must be sent.
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "no SetConfig on second idle tick"
        );
        // The mock response queue is empty; if Stop had been called again the
        // mock would have returned an error and the test would have panicked.

        fs::remove_dir_all(dir).unwrap();
    }

    /// P2 idle invariant — second half:
    /// After an idle Stop, when the source becomes active the controller must
    /// clear the flag and apply the new wave format via SetConfig.
    #[test]
    fn idle_invariant_clears_on_source_active_and_applies_config() {
        let dir = test_dir("idle-invariant-resume");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        // Stop (idle enforcement) + Stop (handle_started) + SetConfig (start)
        let client = MockClient::new(vec![
            MockClient::ok(), // Stop — idle enforcement
            MockClient::ok(), // Stop — handle_started pre-stop
            MockClient::ok(), // SetConfig — handle_started start
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        // Simulate idle enforcement having fired.
        let inactive_snap = MockListener::inactive();
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert!(ctrl.idle_stop_since.is_some());

        // Source becomes active at 48000 / S16_LE / 2.
        let active_snap = DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: Some(48000),
                sample_format: Some("S16_LE".to_owned()),
                channels: Some(2),
            },
        };
        ctrl.handle_started(&active_snap).unwrap();

        assert!(
            ctrl.idle_stop_since.is_none(),
            "guard must be cleared on source active"
        );
        assert_eq!(ctrl.client.sent_configs.len(), 1);
        assert!(
            ctrl.client.sent_configs[0].contains("samplerate: 48000"),
            "SetConfig must use the live 48000 Hz rate"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// P2 idle guard — guard resets after CamillaDSP becomes Inactive.
    ///
    /// Sequence:
    ///   Running / inactive  →  Stop #1, guard armed
    ///   Inactive / inactive →  guard cleared
    ///   Running / inactive  →  Stop #2 (invariant enforced again)
    #[test]
    fn idle_stop_guard_resets_after_inactive() {
        let dir = test_dir("idle-guard-reset");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::ok(),                // Stop #1 (first Running tick)
            MockClient::stop_reason("None"), // GetStopReason (Inactive tick, source inactive)
            MockClient::ok(),                // Stop #2 (second Running tick)
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());
        let inactive_snap = MockListener::inactive();

        // Tick 1: Running + inactive → Stop, guard armed.
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert!(
            ctrl.idle_stop_since.is_some(),
            "guard must be armed after first Stop"
        );

        // Tick 2: Inactive + inactive → guard cleared.
        ctrl.handle_processing_state(ProcessingState::Inactive, &inactive_snap)
            .unwrap();
        assert!(
            ctrl.idle_stop_since.is_none(),
            "guard must be cleared when CamillaDSP reaches Inactive"
        );

        // Tick 3: Running + inactive again → Stop #2 must be sent.
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert!(
            ctrl.idle_stop_since.is_some(),
            "guard must be re-armed after second Stop"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// P2 idle guard — stop is retried after the deadline expires.
    ///
    /// Sequence:
    ///   Running / inactive  →  Stop, guard = now
    ///   Running / inactive within 2 s →  no Stop (rate-limited)
    ///   Manually back-date guard by 3 s
    ///   Running / inactive  →  Stop again (deadline expired)
    #[test]
    fn idle_stop_is_retried_if_cdsp_remains_running() {
        let dir = test_dir("idle-retry");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::ok(), // Stop #1 (first tick)
            // No response consumed on second tick (within deadline).
            MockClient::ok(), // Stop #2 (after deadline)
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());
        let inactive_snap = MockListener::inactive();

        // Tick 1: stop is sent, guard armed.
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert!(ctrl.idle_stop_since.is_some());

        // Tick 2: within 2 s deadline → no second Stop.
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert_eq!(ctrl.client.stop_count, 1, "no second Stop within deadline");

        // Back-date the guard so the deadline appears expired.
        ctrl.idle_stop_since = Some(Instant::now() - Duration::from_secs(IDLE_STOP_RETRY_SECS + 1));

        // Tick 3: deadline expired → Stop re-sent.
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert_eq!(
            ctrl.client.stop_count, 2,
            "Stop must be re-sent after deadline"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// P2 bootstrap — when the controller starts while source is inactive but
    /// CamillaDSP is already Running, bootstrap must immediately Stop it.
    #[test]
    fn bootstrap_stops_running_cdsp_when_source_is_inactive() {
        let dir = test_dir("bootstrap-running-inactive");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::state("Running"), // GetState (bootstrap)
            MockClient::ok(),             // Stop (idle enforcement)
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());
        let inactive_snap = MockListener::inactive();

        ctrl.bootstrap_initial_config(&inactive_snap).unwrap();

        assert!(
            ctrl.idle_stop_since.is_some(),
            "idle guard must be armed after bootstrap Stop"
        );
        assert_eq!(
            ctrl.client.sent_configs.len(),
            0,
            "no SetConfig during bootstrap idle enforcement"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// P2 normal stop — after the ALSA device stops, the idle guard is armed so
    /// that a racing GUI SetConfig cannot leave CamillaDSP running unimpeded
    /// on the very next loop tick.
    #[test]
    fn normal_source_stop_arms_idle_stop_guard() {
        let dir = test_dir("normal-stop-arms-guard");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        // Mock provides one Stop response for the normal device-stop path.
        // No second Stop response is queued; if enforce_idle_invariant were to
        // fire immediately the mock would return an error and the test panics.
        let client = MockClient::new(vec![
            MockClient::ok(), // Stop — normal device stop
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        // Simulate the normal-stop path in run(): stop_cdsp + arm guard.
        ctrl.stop_cdsp().unwrap();
        ctrl.idle_stop_since = Some(Instant::now());
        ctrl.retry.reset();
        ctrl.pending_since = None;

        assert!(
            ctrl.idle_stop_since.is_some(),
            "guard must be armed after normal device stop"
        );

        // A Running tick arriving within 2 s must NOT send another Stop.
        let inactive_snap = MockListener::inactive();
        ctrl.handle_processing_state(ProcessingState::Running, &inactive_snap)
            .unwrap();
        assert_eq!(
            ctrl.client.stop_count, 1,
            "no extra Stop must be sent within the guard window"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Gate 0 acceptance: 44.1 → 48 → 96 kHz transitions each trigger an
    /// adapted restart with the live source sample-rate.
    #[test]
    fn acceptance_rate_change_sequence_reapplies_live_rates() {
        let dir = test_dir("acceptance-rate-sequence");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::ok(), // Stop 44.1
            MockClient::ok(), // SetConfig 44.1
            MockClient::ok(), // Stop 48
            MockClient::ok(), // SetConfig 48
            MockClient::ok(), // Stop 96
            MockClient::ok(), // SetConfig 96
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        ctrl.handle_started(&MockListener::active_with_rate(44100))
            .unwrap();
        ctrl.handle_started(&MockListener::active_with_rate(48000))
            .unwrap();
        ctrl.handle_started(&MockListener::active_with_rate(96000))
            .unwrap();

        assert_eq!(ctrl.client.sent_configs.len(), 3);
        assert!(ctrl.client.sent_configs[0].contains("samplerate: 44100"));
        assert!(ctrl.client.sent_configs[1].contains("samplerate: 48000"));
        assert!(ctrl.client.sent_configs[2].contains("samplerate: 96000"));

        fs::remove_dir_all(dir).unwrap();
    }

    /// Gate 0 acceptance: format changes are reflected in runtime config
    /// adaptation for the next start.
    #[test]
    fn acceptance_format_change_reapplies_live_format() {
        let dir = test_dir("acceptance-format-change");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(
            &config,
            "devices:\n  samplerate: 44100\n  chunksize: 2048\n  \
             capture:\n    type: Alsa\n    channels: 2\n    \
             device: \"hw:Loopback,0,0\"\n    format: S32_LE\n  \
             playback:\n    type: Alsa\n    channels: 2\n    \
             device: \"hw:X,0\"\n\
             filters: {}\nmixers: {}\npipeline: []\nprocessors: {}\n",
        )
        .unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::ok(), // Stop first start
            MockClient::ok(), // SetConfig first start
            MockClient::ok(), // Stop second start
            MockClient::ok(), // SetConfig second start
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        let first = DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: Some(48000),
                sample_format: Some("S16_LE".to_owned()),
                channels: Some(2),
            },
        };
        let second = DeviceSnapshot {
            active: true,
            wave: WaveFormat {
                sample_rate: Some(48000),
                sample_format: Some("S24_3_LE".to_owned()),
                channels: Some(2),
            },
        };

        ctrl.handle_started(&first).unwrap();
        ctrl.handle_started(&second).unwrap();

        assert_eq!(ctrl.client.sent_configs.len(), 2);
        assert_ne!(
            ctrl.client.sent_configs[0], ctrl.client.sent_configs[1],
            "format change must produce a different adapted config payload"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// Gate 0 acceptance: if CamillaDSP becomes Inactive while playback is
    /// still active, controller immediately re-applies runtime config.
    #[test]
    fn acceptance_cdsp_restart_with_active_source_restarts_processing() {
        let dir = test_dir("acceptance-cdsp-restart");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::stop_reason("None"), // GetStopReason from Inactive state
            MockClient::ok(),                // SetConfig restart
        ]);
        let listener = MockListener::new(vec![MockListener::active_with_rate(48000)]);
        let mut ctrl = make_controller(client, listener, active.clone());

        let snap = MockListener::active_with_rate(48000);
        ctrl.process_inactive_state(&snap).unwrap();

        assert_eq!(ctrl.client.sent_configs.len(), 1);

        fs::remove_dir_all(dir).unwrap();
    }

    /// Gate 0 acceptance: transient WebSocket transport errors during restart
    /// are surfaced as controller errors for supervisor-level recovery.
    #[test]
    fn acceptance_transient_websocket_failure_returns_error() {
        let dir = test_dir("acceptance-ws-failure");
        let config = dir.join("config.yml");
        let active = dir.join("active.yml");
        fs::write(&config, minimal_config("hw:X,0")).unwrap();
        symlink(&config, &active).unwrap();

        let client = MockClient::new(vec![
            MockClient::ok(), // Stop from handle_started
            Err(WsError::Transport("temporary network glitch".to_owned())),
        ]);
        let mut ctrl = make_controller(client, MockListener::new(vec![]), active.clone());

        let result = ctrl.handle_started(&MockListener::active_with_rate(48000));
        assert!(result.is_err(), "transport failure must be returned");
        let message = format!("{}", result.unwrap_err());
        assert!(
            message.contains("temporary network glitch"),
            "error must surface the underlying transport failure"
        );

        fs::remove_dir_all(dir).unwrap();
    }
}
