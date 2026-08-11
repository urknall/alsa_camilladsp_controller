use crate::adapt::adapt_config;
use crate::alsa_listener::AlsaLoopbackListener;
use crate::args::Args;
use crate::camilla_ws::{
    parse_processing_state, parse_stop_reason, CamillaWs, ProcessingState, StopReason, WsError,
};
use crate::error::{app_error, AppResult};
use crate::logging::{log, LogLevel};
use crate::wave::{DeviceSnapshot, WaveFormat};
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Matches the Python listener's 50 ms debounce before reading ALSA controls.
const ALSA_DEBOUNCE_MS: u64 = 50;
/// Matches the Python controller's 200 ms event-queue poll interval.
const CONTROL_LOOP_MS: u32 = 200;

/// Ties together the ALSA listener and the CamillaDSP WebSocket client,
/// implementing the control loop that mirrors the Python reference controller's
/// `--adapt` behavior.
pub struct Controller {
    client: CamillaWs,
    listener: AlsaLoopbackListener,
    adapt_path: PathBuf,
    fallback_wave: WaveFormat,
    config: Option<String>,
    error_on_start: bool,
    log_level: LogLevel,
}

impl Controller {
    /// Create a controller from the parsed CLI arguments, connect to
    /// CamillaDSP, and perform the initial adaptation so the correct config
    /// is ready before the first `start_cdsp()` call.
    pub fn new(args: &Args) -> AppResult<(Self, DeviceSnapshot)> {
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
        // current loopback rate/format/channels immediately, before the first
        // processing start.  This ensures the correct config is sent even when
        // audio is already playing when the controller starts.
        let effective = initial.wave.with_fallback(&controller.fallback_wave);
        controller.refresh_config(&effective);
        Ok((controller, initial))
    }

    /// Re-read and adapt the active config file for `wave`, storing the
    /// result in `self.config`.  On error, sets `self.config = None` and logs
    /// a warning so the controller degrades gracefully rather than crashing.
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
                // remembered and not retried continuously until a new ALSA event.
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
                        "CamillaDSP stopped because capture format changed \
                         (reported rate {reported_rate})"
                    ),
                );
                // Re-read the loopback snapshot for fresh format/channels.
                // Fall back to the CamillaDSP-reported rate only when the
                // loopback control has not yet updated.
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
            _ => {} // Catch guarded arms when error_on_start is true.
        }
        Ok(())
    }

    /// Run the main control loop until an unrecoverable error occurs.
    ///
    /// Loop structure (matches the Python reference):
    /// 1. Wait on the HCTL file descriptor for up to `CONTROL_LOOP_MS`.
    /// 2. If an event fired: sleep 50 ms to debounce, drain kernel event buffer.
    /// 3. Read a fresh snapshot unconditionally.
    /// 4. Handle active/inactive transitions and wave-format changes.
    /// 5. Query CamillaDSP state and handle `Inactive` if needed.
    pub fn run(mut self, mut previous: DeviceSnapshot) -> AppResult<()> {
        log(
            LogLevel::Info,
            self.log_level,
            "Starting ALSA loopback controller",
        );
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
                // Mirrors the Python listener's STOPPED-then-STARTED pair.
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
