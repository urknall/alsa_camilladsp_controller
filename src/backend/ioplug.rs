use crate::backend::{
    AudioTransport, BackendProfile, ControllerBackend, StreamDetector, StreamEvent,
};
use crate::camilladsp::alsa_capture::alsa_format_to_camilladsp;
use crate::core::config::{DeviceSnapshot, WaveFormat};
use crate::core::errors::{app_error, AppResult};
use crate::core::logging::{log, LogLevel};
use crate::ipc::protocol::{ErrorCode, PluginMessage, ProtocolError};
use crate::ipc::unix_socket::{IpcConnection, IpcServer, IpcServerConfig};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::thread;
use std::time::Duration;

// ─── Internal state machine ────────────────────────────────────────────────

enum IoplugState {
    /// No plugin connected; IpcServer is waiting for an incoming connection.
    Idle,
    /// A plugin has sent START and is blocked waiting for READY or ERROR.
    /// The controller must call `on_stream_ready` (or `send_error_to_plugin`)
    /// before the next `poll_event`.
    AwaitingAck {
        conn: IpcConnection,
        snapshot: DeviceSnapshot,
    },
    /// READY has been sent; plugin is streaming.  Waiting for STOP or
    /// disconnect.
    Active {
        conn: IpcConnection,
        snapshot: DeviceSnapshot,
    },
}

// ─── Backend ──────────────────────────────────────────────────────────────

/// ioplug stream backend: drives the plugin ↔ controller IPC handshake and
/// exposes stream lifecycle events to the controller state machine.
///
/// Gate 7 implements the START → adapt → READY handshake.  Gate 8 extends
/// `on_stream_ready` to pass a pipe write-fd to the plugin via SCM_RIGHTS.
pub struct IoplugBackend {
    server: IpcServer,
    state: IoplugState,
    /// Snapshot returned while the stream is idle.
    idle_snapshot: DeviceSnapshot,
    log_level: LogLevel,
}

impl IoplugBackend {
    /// Bind the AF_UNIX IPC socket and return a ready-to-accept backend.
    pub fn new(socket_path: impl AsRef<Path>, log_level: LogLevel) -> AppResult<Self> {
        let server = IpcServer::bind(socket_path, IpcServerConfig::default())?;
        Ok(Self {
            server,
            state: IoplugState::Idle,
            idle_snapshot: DeviceSnapshot {
                active: false,
                wave: WaveFormat::default(),
            },
            log_level,
        })
    }

    /// Notify the plugin that CamillaDSP is ready.  Transitions the backend
    /// from `AwaitingAck` → `Active`.
    ///
    /// For Gate 7 this sends a plain `READY` message (no pipe fd).  Gate 8
    /// will extend this to also pass the pipe write-end via `SCM_RIGHTS`.
    pub fn send_ready_to_plugin(&mut self) -> AppResult<()> {
        let prev = std::mem::replace(&mut self.state, IoplugState::Idle);
        match prev {
            IoplugState::AwaitingAck { mut conn, snapshot } => {
                conn.send_ready()
                    .map_err(|e| app_error(format!("IPC send READY: {e}")))?;
                log(
                    LogLevel::Info,
                    self.log_level,
                    "ioplug: sent READY to plugin",
                );
                self.state = IoplugState::Active { conn, snapshot };
                Ok(())
            }
            other => {
                self.state = other;
                Err(app_error(
                    "send_ready_to_plugin called but backend is not in AwaitingAck state",
                ))
            }
        }
    }

    /// Send READY to the plugin and deliver `pipe_write_fd` via SCM_RIGHTS.
    ///
    /// This is the Gate 8 variant of `send_ready_to_plugin`: the plugin will
    /// receive the pipe write-end and write raw PCM into it directly, without
    /// Rust being in the data path.
    ///
    /// Transitions the backend from `AwaitingAck` → `Active`.
    pub fn send_ready_with_fd_to_plugin(&mut self, pipe_write_fd: RawFd) -> AppResult<()> {
        let prev = std::mem::replace(&mut self.state, IoplugState::Idle);
        match prev {
            IoplugState::AwaitingAck { mut conn, snapshot } => {
                conn.send_ready_with_pipe_fd(pipe_write_fd)
                    .map_err(|e| app_error(format!("IPC send READY+fd: {e}")))?;
                log(
                    LogLevel::Info,
                    self.log_level,
                    format!("ioplug: sent READY with pipe_fd={pipe_write_fd} to plugin"),
                );
                self.state = IoplugState::Active { conn, snapshot };
                Ok(())
            }
            other => {
                self.state = other;
                Err(app_error(
                    "send_ready_with_fd_to_plugin called but backend is not in AwaitingAck state",
                ))
            }
        }
    }

    /// Notify the plugin of a controller-side error and reset to `Idle`.
    pub fn send_error_to_plugin(&mut self, code: ErrorCode) {
        let prev = std::mem::replace(&mut self.state, IoplugState::Idle);
        if let IoplugState::AwaitingAck { mut conn, .. } = prev {
            let _ = conn.send_error(code);
            log(
                LogLevel::Warning,
                self.log_level,
                format!("ioplug: sent ERROR {:?} to plugin", code),
            );
        }
        // state is now Idle regardless
    }

    // ── private helpers ─────────────────────────────────────────────────

    /// Attempt to accept an incoming plugin connection without blocking.
    /// On success performs the HELLO handshake and reads the START message.
    /// Returns `None` if no client is waiting.
    fn try_accept_and_start(&mut self) -> AppResult<Option<(IpcConnection, DeviceSnapshot)>> {
        let conn = match self.server.try_accept()? {
            Some(c) => c,
            None => return Ok(None),
        };
        self.handshake_and_start(conn).map(Some)
    }

    fn handshake_and_start(
        &mut self,
        mut conn: IpcConnection,
    ) -> AppResult<(IpcConnection, DeviceSnapshot)> {
        // HELLO negotiation.
        conn.perform_hello_handshake()
            .map_err(|e| app_error(format!("IPC HELLO failed: {e}")))?;

        log(
            LogLevel::Debug,
            self.log_level,
            "ioplug: HELLO handshake complete",
        );

        // Receive the first plugin message; must be START.
        let msg = conn
            .recv_plugin_message()
            .map_err(|e| app_error(format!("IPC recv after HELLO: {e}")))?;

        let snapshot = match msg {
            PluginMessage::Start {
                rate,
                format,
                channels,
                ..
            } => {
                let fmt_str = alsa_format_to_camilladsp(format as i32)
                    .map_err(|e| app_error(format!("IPC START format {format}: {e}")))?
                    .ok_or_else(|| {
                        app_error(format!("IPC START: unsupported ALSA format byte {format}"))
                    })?;

                log(
                    LogLevel::Info,
                    self.log_level,
                    format!(
                        "ioplug: received START rate={rate} format={fmt_str} channels={channels}"
                    ),
                );

                DeviceSnapshot {
                    active: true,
                    wave: WaveFormat {
                        sample_rate: Some(rate),
                        sample_format: Some(fmt_str.to_owned()),
                        channels: Some(channels as u32),
                    },
                }
            }
            other => {
                return Err(app_error(format!(
                    "IPC expected START, got {:?}",
                    other.message_type()
                )));
            }
        };

        Ok((conn, snapshot))
    }

    /// Receive one message from an active connection.  Returns `Ok(None)` on
    /// timeout.
    fn recv_from_active(conn: &mut IpcConnection) -> Result<Option<PluginMessage>, ProtocolError> {
        match conn.recv_plugin_message() {
            Ok(msg) => Ok(Some(msg)),
            Err(ProtocolError::Timeout) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

impl ControllerBackend for IoplugBackend {
    fn poll_event(&mut self, timeout_ms: u32) -> AppResult<Option<StreamEvent>> {
        match &mut self.state {
            IoplugState::Idle => {
                // Non-blocking accept attempt, then short sleep to avoid busy-wait.
                let result = self.try_accept_and_start()?;
                if let Some((conn, snapshot)) = result {
                    let wave = snapshot.wave.clone();
                    let event =
                        StreamEvent::Started(crate::backend::StreamParams::from_wave(&wave)?);
                    self.state = IoplugState::AwaitingAck { conn, snapshot };
                    return Ok(Some(event));
                }
                // No client yet; sleep a fraction of the timeout to back-off.
                let sleep_ms = (timeout_ms / 4).clamp(1, 50);
                thread::sleep(Duration::from_millis(sleep_ms as u64));
                Ok(None)
            }

            IoplugState::AwaitingAck { .. } => {
                // The controller must call on_stream_ready / send_error_to_plugin
                // before polling again.  Return None to let the loop tick.
                thread::sleep(Duration::from_millis(10));
                Ok(None)
            }

            IoplugState::Active { conn, .. } => match Self::recv_from_active(conn) {
                Ok(Some(PluginMessage::Stop { .. })) => {
                    log(
                        LogLevel::Info,
                        self.log_level,
                        "ioplug: received STOP from plugin",
                    );
                    self.state = IoplugState::Idle;
                    Ok(Some(StreamEvent::Stopped))
                }
                Ok(Some(other)) => {
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        format!(
                            "ioplug: unexpected message {:?} in Active state — ignoring",
                            other.message_type()
                        ),
                    );
                    Ok(None)
                }
                Ok(None) => Ok(None),
                Err(ProtocolError::Disconnected) => {
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        "ioplug: plugin disconnected unexpectedly",
                    );
                    self.state = IoplugState::Idle;
                    Ok(Some(StreamEvent::Stopped))
                }
                Err(err) => {
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        format!("ioplug: IPC error in Active state: {err} — treating as stop"),
                    );
                    self.state = IoplugState::Idle;
                    Ok(Some(StreamEvent::Stopped))
                }
            },
        }
    }

    fn current_snapshot(&self) -> &DeviceSnapshot {
        match &self.state {
            IoplugState::Idle => &self.idle_snapshot,
            IoplugState::AwaitingAck { snapshot, .. } | IoplugState::Active { snapshot, .. } => {
                snapshot
            }
        }
    }

    fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
        Ok(self.current_snapshot().clone())
    }

    /// Send READY to the plugin, releasing it to start PCM transfer.
    fn on_stream_ready(&mut self) -> AppResult<()> {
        self.send_ready_to_plugin()
    }
}

impl BackendProfile for IoplugBackend {
    fn detector(&self) -> StreamDetector {
        StreamDetector::IoplugIpc
    }

    fn transport(&self) -> AudioTransport {
        AudioTransport::StdinPipe
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::protocol::PluginMessage;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_socket_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "picoredsp-ioplug-{name}-{}-{nanos}.sock",
            std::process::id()
        ))
    }

    fn connect_and_hello(path: &std::path::Path) -> UnixStream {
        let mut client = UnixStream::connect(path).unwrap();
        client
            .write_all(&PluginMessage::Hello { version: 1 }.encode())
            .unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(
            crate::ipc::protocol::PluginMessage::decode(&reply).unwrap(),
            PluginMessage::Hello { version: 1 }
        );
        client
    }

    #[test]
    fn idle_poll_returns_none_when_no_client() {
        let path = test_socket_path("idle-none");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();
        let result = backend.poll_event(50).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn poll_returns_started_on_valid_start_message() {
        let path = test_socket_path("started");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();

        let path2 = path.clone();
        let client_handle = std::thread::spawn(move || {
            // Give the test server a moment to bind.
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&path2);
            client
                .write_all(
                    &PluginMessage::Start {
                        version: 1,
                        rate: 48_000,
                        format: 10,
                        channels: 2,
                    }
                    .encode(),
                )
                .unwrap();
            client
        });

        let event = loop {
            match backend.poll_event(200).unwrap() {
                Some(e) => break e,
                None => {}
            }
        };

        assert_eq!(
            event,
            StreamEvent::Started(crate::backend::StreamParams {
                rate: 48_000,
                format: "S32_LE".to_owned(),
                channels: 2,
            })
        );

        let snap = backend.current_snapshot();
        assert!(snap.active);
        assert_eq!(snap.wave.sample_rate, Some(48_000));
        assert_eq!(snap.wave.sample_format.as_deref(), Some("S32_LE"));
        assert_eq!(snap.wave.channels, Some(2));

        let _ = client_handle.join();
    }

    #[test]
    fn on_stream_ready_sends_ready_to_plugin() {
        let path = test_socket_path("ready");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();

        let path2 = path.clone();
        let client_handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&path2);
            client
                .write_all(
                    &PluginMessage::Start {
                        version: 1,
                        rate: 44_100,
                        format: 2,
                        channels: 2,
                    }
                    .encode(),
                )
                .unwrap();
            let mut ready_buf = [0u8; 2];
            client.read_exact(&mut ready_buf).unwrap();
            let msg = PluginMessage::decode(&ready_buf).unwrap();
            assert_eq!(msg, PluginMessage::Ready { version: 1 });
            client
        });

        loop {
            if backend.poll_event(200).unwrap().is_some() {
                break;
            }
        }
        backend.on_stream_ready().unwrap();

        assert!(backend.current_snapshot().active);

        let _ = client_handle.join();
    }

    #[test]
    fn poll_returns_stopped_on_stop_message() {
        let path = test_socket_path("stop");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();

        let path2 = path.clone();
        let client_handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&path2);
            client
                .write_all(
                    &PluginMessage::Start {
                        version: 1,
                        rate: 96_000,
                        format: 10,
                        channels: 2,
                    }
                    .encode(),
                )
                .unwrap();
            let mut buf = [0u8; 2];
            client.read_exact(&mut buf).unwrap();
            client
                .write_all(&PluginMessage::Stop { version: 1 }.encode())
                .unwrap();
        });

        loop {
            if backend.poll_event(200).unwrap().is_some() {
                break;
            }
        }
        backend.on_stream_ready().unwrap();

        let event = loop {
            match backend.poll_event(200).unwrap() {
                Some(e) => break e,
                None => {}
            }
        };
        assert_eq!(event, StreamEvent::Stopped);
        assert!(!backend.current_snapshot().active);

        let _ = client_handle.join();
    }

    #[test]
    fn poll_returns_stopped_on_plugin_disconnect() {
        let path = test_socket_path("disconnect");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();

        let path2 = path.clone();
        let client_handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&path2);
            client
                .write_all(
                    &PluginMessage::Start {
                        version: 1,
                        rate: 48_000,
                        format: 10,
                        channels: 2,
                    }
                    .encode(),
                )
                .unwrap();
            let mut buf = [0u8; 2];
            client.read_exact(&mut buf).unwrap();
            drop(client);
        });

        loop {
            if backend.poll_event(200).unwrap().is_some() {
                break;
            }
        }
        backend.on_stream_ready().unwrap();

        let event = loop {
            match backend.poll_event(200).unwrap() {
                Some(e) => break e,
                None => {}
            }
        };
        assert_eq!(event, StreamEvent::Stopped);

        let _ = client_handle.join();
    }

    #[test]
    fn send_error_to_plugin_sends_error_and_resets_to_idle() {
        let path = test_socket_path("error");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();

        let path2 = path.clone();
        let client_handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&path2);
            client
                .write_all(
                    &PluginMessage::Start {
                        version: 1,
                        rate: 48_000,
                        format: 10,
                        channels: 2,
                    }
                    .encode(),
                )
                .unwrap();
            let mut buf = [0u8; 3];
            client.read_exact(&mut buf).unwrap();
            let msg = PluginMessage::decode(&buf).unwrap();
            assert_eq!(
                msg,
                PluginMessage::Error {
                    version: 1,
                    code: crate::ipc::protocol::ErrorCode::Config
                }
            );
        });

        loop {
            if backend.poll_event(200).unwrap().is_some() {
                break;
            }
        }
        backend.send_error_to_plugin(ErrorCode::Config);
        assert!(!backend.current_snapshot().active);

        let _ = client_handle.join();
    }

    #[test]
    fn profile_reports_ioplug_detector_and_stdin_transport() {
        let path = test_socket_path("profile");
        let backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();
        assert_eq!(backend.detector(), StreamDetector::IoplugIpc);
        assert_eq!(backend.transport(), AudioTransport::StdinPipe);
    }
}
