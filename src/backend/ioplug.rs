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

    /// Test-only helper for Gate 7 style unit tests that still exercise the
    /// plain `READY` handshake without passing a pipe fd.
    #[cfg(test)]
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
        let mut conn = match self.server.try_accept()? {
            Some(c) => c,
            None => return Ok(None),
        };

        // HELLO negotiation — stores negotiated_version on `conn`.
        let negotiated = match conn.perform_hello_handshake() {
            Ok(version) => version,
            Err(err) => {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("ioplug: rejecting client during HELLO: {err}"),
                );
                return Ok(None);
            }
        };

        log(
            LogLevel::Debug,
            self.log_level,
            "ioplug: HELLO handshake complete",
        );

        // Receive the first plugin message; must be START.
        let msg = match conn.recv_plugin_message() {
            Ok(msg) => msg,
            Err(err) => {
                let _ = conn.send_error(ErrorCode::Protocol);
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("ioplug: rejecting client after HELLO: {err}"),
                );
                return Ok(None);
            }
        };

        let snapshot = match msg {
            PluginMessage::Start {
                version,
                rate,
                format,
                channels,
            } => {
                // Fix: reject any post-HELLO message whose version does not
                // match the negotiated version.  Both sides agreed on
                // `negotiated` during HELLO; a different version here means
                // the peer violated the protocol.
                if version != negotiated {
                    let _ = conn.send_error(ErrorCode::Protocol);
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        format!(
                            "ioplug: rejecting client START with version mismatch \
                             (negotiated {negotiated}, got {version})"
                        ),
                    );
                    return Ok(None);
                }

                // Fix: enforce the stereo-only contract at every trust
                // boundary.  The C plugin ALSA constraint already limits
                // negotiation to 2 channels, but validate here as a
                // defence-in-depth check (e.g. future clients, tests).
                if channels != 2 {
                    let _ = conn.send_error(ErrorCode::Protocol);
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        format!(
                            "ioplug: rejecting client START with unsupported channel count {channels}"
                        ),
                    );
                    return Ok(None);
                }

                let fmt_str = match alsa_format_to_camilladsp(format as i32) {
                    Ok(Some(fmt)) => fmt,
                    Ok(None) => {
                        let _ = conn.send_error(ErrorCode::Protocol);
                        log(
                            LogLevel::Warning,
                            self.log_level,
                            format!(
                                "ioplug: rejecting client START with unsupported ALSA format byte {format}"
                            ),
                        );
                        return Ok(None);
                    }
                    Err(err) => {
                        let _ = conn.send_error(ErrorCode::Protocol);
                        log(
                            LogLevel::Warning,
                            self.log_level,
                            format!("ioplug: rejecting client START format {format}: {err}"),
                        );
                        return Ok(None);
                    }
                };

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
                let _ = conn.send_error(ErrorCode::Protocol);
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!(
                        "ioplug: rejecting client after HELLO: expected START, got {:?}",
                        other.message_type()
                    ),
                );
                return Ok(None);
            }
        };

        Ok(Some((conn, snapshot)))
    }

    /// Receive one message from an active connection.  Returns `Ok(None)` on
    /// timeout.  Validates that any received message carries the negotiated
    /// protocol version.
    fn recv_from_active(conn: &mut IpcConnection) -> Result<Option<PluginMessage>, ProtocolError> {
        let msg = match conn.recv_plugin_message() {
            Ok(m) => m,
            Err(ProtocolError::Timeout) => return Ok(None),
            Err(err) => return Err(err),
        };
        // Enforce the negotiated version on every post-HELLO message.
        // A peer that changes its version mid-session violates the protocol.
        if let Some(negotiated) = conn.negotiated_version() {
            let actual = msg.version();
            if actual != negotiated {
                return Err(ProtocolError::VersionMismatch {
                    expected: negotiated,
                    actual,
                });
            }
        }
        Ok(Some(msg))
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
                    let _ = conn.send_error(ErrorCode::Protocol);
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        format!(
                            "ioplug: unexpected message {:?} in Active state — protocol violation",
                            other.message_type()
                        ),
                    );
                    self.state = IoplugState::Idle;
                    Ok(Some(StreamEvent::Stopped))
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
                    let _ = conn.send_error(ErrorCode::Protocol);
                    log(
                        LogLevel::Warning,
                        self.log_level,
                        format!("ioplug: protocol error in Active state: {err} — closing stream"),
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

    /// Called by the `ControllerBackend` trait's generic `on_stream_ready` hook.
    ///
    /// For the ioplug backend this path is **not supported**: the plugin
    /// requires a pipe write-end delivered via SCM_RIGHTS, so callers must
    /// always use `send_ready_with_fd_to_plugin` directly.  Returning an
    /// explicit error here prevents a future refactor from accidentally
    /// activating the no-fd path through the generic trait.
    fn on_stream_ready(&mut self) -> AppResult<()> {
        Err(app_error(
            "ioplug: on_stream_ready is not supported; \
             call send_ready_with_fd_to_plugin with a pipe fd instead",
        ))
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
    use crate::camilladsp::supervisor::StdinSupervisor;
    use crate::ipc::protocol::PluginMessage;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
            if let Some(e) = backend.poll_event(200).unwrap() {
                break e;
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
    fn malformed_client_does_not_poison_listener_for_next_client() {
        let path = test_socket_path("malformed-then-valid");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();

        let bad_path = path.clone();
        let bad_client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&bad_path);
            client
                .write_all(&PluginMessage::Stop { version: 1 }.encode())
                .unwrap();
            let mut err_buf = [0u8; 3];
            client.read_exact(&mut err_buf).unwrap();
            assert_eq!(
                PluginMessage::decode(&err_buf).unwrap(),
                PluginMessage::Error {
                    version: 1,
                    code: ErrorCode::Protocol,
                }
            );
        });

        for _ in 0..10 {
            assert!(backend.poll_event(50).unwrap().is_none());
        }
        bad_client.join().unwrap();

        let good_path = path.clone();
        let good_client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&good_path);
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
            if let Some(event) = backend.poll_event(200).unwrap() {
                break event;
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

        let _ = good_client.join();
    }

    #[test]
    fn on_stream_ready_returns_error_for_ioplug() {
        // on_stream_ready must NOT silently send plain READY for the ioplug
        // backend.  The plugin expects a pipe write-end delivered via
        // SCM_RIGHTS; only send_ready_with_fd_to_plugin provides that.
        // Returning an explicit error prevents accidental use of the plain-READY
        // path through the generic ControllerBackend trait.
        let path = test_socket_path("on-ready-error");
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
            // Keep the socket open so the backend doesn't see a disconnect.
            std::thread::sleep(Duration::from_millis(100));
        });

        loop {
            if backend.poll_event(200).unwrap().is_some() {
                break;
            }
        }
        let err = backend.on_stream_ready().unwrap_err();
        assert!(
            err.to_string().contains("send_ready_with_fd_to_plugin"),
            "error should mention the correct API, got: {err}"
        );

        let _ = client_handle.join();
    }

    #[test]
    fn send_ready_to_plugin_sends_ready_to_plugin() {
        let path = test_socket_path("send-ready-direct");
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
        backend.send_ready_to_plugin().unwrap();

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
        backend.send_ready_to_plugin().unwrap();

        let event = loop {
            if let Some(e) = backend.poll_event(200).unwrap() {
                break e;
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
        backend.send_ready_to_plugin().unwrap();

        let event = loop {
            if let Some(e) = backend.poll_event(200).unwrap() {
                break e;
            }
        };
        assert_eq!(event, StreamEvent::Stopped);

        let _ = client_handle.join();
    }

    fn assert_active_message_is_protocol_violation(message: PluginMessage, test_name: &str) {
        let path = test_socket_path(test_name);
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
            let mut ready = [0u8; 2];
            client.read_exact(&mut ready).unwrap();
            assert_eq!(
                PluginMessage::decode(&ready).unwrap(),
                PluginMessage::Ready { version: 1 }
            );
            client.write_all(&message.encode()).unwrap();
            let mut err_buf = [0u8; 3];
            client.read_exact(&mut err_buf).unwrap();
            assert_eq!(
                PluginMessage::decode(&err_buf).unwrap(),
                PluginMessage::Error {
                    version: 1,
                    code: ErrorCode::Protocol,
                }
            );
        });

        loop {
            if backend.poll_event(200).unwrap().is_some() {
                break;
            }
        }
        backend.send_ready_to_plugin().unwrap();

        let event = loop {
            if let Some(e) = backend.poll_event(200).unwrap() {
                break e;
            }
        };
        assert_eq!(event, StreamEvent::Stopped);
        assert!(!backend.current_snapshot().active);

        client_handle.join().unwrap();
    }

    #[test]
    fn active_state_rejects_version_mismatch_on_hello_ready_and_error_frames() {
        for (name, message) in [
            (
                "active-hello-version-mismatch",
                PluginMessage::Hello { version: 2 },
            ),
            (
                "active-ready-version-mismatch",
                PluginMessage::Ready { version: 2 },
            ),
            (
                "active-error-version-mismatch",
                PluginMessage::Error {
                    version: 2,
                    code: ErrorCode::Config,
                },
            ),
        ] {
            assert_active_message_is_protocol_violation(message, name);
        }
    }

    #[test]
    fn active_state_treats_unexpected_version_correct_message_as_protocol_violation() {
        assert_active_message_is_protocol_violation(
            PluginMessage::Hello { version: 1 },
            "active-unexpected-hello",
        );
    }

    #[test]
    fn backend_supports_two_full_ioplug_stream_chains_in_sequence() {
        let path = test_socket_path("two-stream-chains");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();

        let first_path = path.clone();
        let first_client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&first_path);
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
            let mut ready = [0u8; 2];
            client.read_exact(&mut ready).unwrap();
            assert_eq!(
                PluginMessage::decode(&ready).unwrap(),
                PluginMessage::Ready { version: 1 }
            );
            client
                .write_all(&PluginMessage::Stop { version: 1 }.encode())
                .unwrap();
        });

        let first_started = loop {
            if let Some(event) = backend.poll_event(200).unwrap() {
                break event;
            }
        };
        assert_eq!(
            first_started,
            StreamEvent::Started(crate::backend::StreamParams {
                rate: 48_000,
                format: "S32_LE".to_owned(),
                channels: 2,
            })
        );
        backend.send_ready_to_plugin().unwrap();
        let first_stopped = loop {
            if let Some(event) = backend.poll_event(200).unwrap() {
                break event;
            }
        };
        assert_eq!(first_stopped, StreamEvent::Stopped);
        assert!(!backend.current_snapshot().active);
        first_client.join().unwrap();

        let second_path = path.clone();
        let second_client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut client = connect_and_hello(&second_path);
            client
                .write_all(
                    &PluginMessage::Start {
                        version: 1,
                        rate: 96_000,
                        format: 2,
                        channels: 2,
                    }
                    .encode(),
                )
                .unwrap();
            let mut ready = [0u8; 2];
            client.read_exact(&mut ready).unwrap();
            assert_eq!(
                PluginMessage::decode(&ready).unwrap(),
                PluginMessage::Ready { version: 1 }
            );
            drop(client);
        });

        let second_started = loop {
            if let Some(event) = backend.poll_event(200).unwrap() {
                break event;
            }
        };
        assert_eq!(
            second_started,
            StreamEvent::Started(crate::backend::StreamParams {
                rate: 96_000,
                format: "S16_LE".to_owned(),
                channels: 2,
            })
        );
        backend.send_ready_to_plugin().unwrap();
        let second_stopped = loop {
            if let Some(event) = backend.poll_event(200).unwrap() {
                break event;
            }
        };
        assert_eq!(second_stopped, StreamEvent::Stopped);
        assert!(!backend.current_snapshot().active);
        second_client.join().unwrap();
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

    // ── Failure scenario tests (M10 checklist) ────────────────────────────

    /// M10 failure scenario: "Plugin/application disappears: Rust cleans up
    /// CamillaDSP (control socket close + PCM fd close)".
    ///
    /// Verify the full Rust side of the scenario end-to-end:
    /// 1. Plugin connects and sends START → backend emits `Started`.
    /// 2. Controller acknowledges with READY.
    /// 3. Plugin drops the socket (application crash / process exit).
    /// 4. `poll_event` detects the disconnect and returns `Stopped`.
    /// 5. Controller calls `supervisor.stop_stream()` to close the pipe
    ///    write-end → the spawned process sees EOF and exits.
    /// 6. `supervisor.is_running()` returns `false`.
    #[test]
    fn failure_plugin_disappears_supervisor_cleans_up_camilladsp() {
        let path = test_socket_path("disappear-cleanup");
        let mut backend = IoplugBackend::new(&path, LogLevel::Error).unwrap();

        // Write a tiny dummy config for the supervisor (cat ignores its args).
        let cfg =
            std::env::temp_dir().join(format!("picoredsp-disappear-{}.txt", std::process::id()));
        std::fs::write(&cfg, "dummy").unwrap();
        let mut supervisor = StdinSupervisor::new("/bin/cat", &cfg, LogLevel::Error);

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
            // Wait for READY (2 bytes).
            let mut buf = [0u8; 2];
            client.read_exact(&mut buf).unwrap();
            // Drop the socket — simulate application crash.
            drop(client);
        });

        // Wait for Started event.
        loop {
            if backend.poll_event(200).unwrap().is_some() {
                break;
            }
        }

        // Start CamillaDSP (use `/bin/cat` as a stand-in).
        let write_fd = supervisor.start_stream().unwrap();
        assert!(supervisor.is_running(), "cat should be running");

        // Send READY to plugin (plain, no SCM_RIGHTS for this test).
        backend.send_ready_to_plugin().unwrap();

        // Wait for Stopped event (plugin disconnected).
        let event = loop {
            if let Some(e) = backend.poll_event(200).unwrap() {
                break e;
            }
        };
        assert_eq!(event, StreamEvent::Stopped);

        // Controller cleans up: close our write-end of the pipe.
        // When `cat` has no more data and its stdin is closed it exits.
        supervisor.stop_stream();
        // Closing the write-end is enough for `cat` to exit.
        assert!(
            !supervisor.is_running(),
            "cat should have exited after pipe write-end was closed"
        );

        let _ = client_handle.join();
        let _ = std::fs::remove_file(&cfg);
        // Suppress unused-variable warning — write_fd was returned by start_stream.
        let _ = write_fd;
    }

    /// M10 failure scenario: "Rust daemon restarts mid-stream: active stream
    /// fails cleanly (reconnect not required for v1)".
    ///
    /// When Rust restarts its `StdinSupervisor` is dropped, which closes
    /// Rust's write-end and kills CamillaDSP.  The plugin still holds its
    /// copy of the pipe write-end (received via SCM_RIGHTS), but once
    /// CamillaDSP is gone its next `write()` into the pipe returns EPIPE.
    /// The worker thread detects EPIPE and stops → the stream "fails cleanly".
    ///
    /// This test verifies the EPIPE signal path using a manual pipe pair:
    /// 1. Create a pipe (read-end = "CamillaDSP stdin", write-end = "plugin").
    /// 2. Close the read-end (simulating CamillaDSP exiting after Rust restart).
    /// 3. Write to the write-end → must get EPIPE.
    ///
    /// The supervisor's cleanup path (killing the process) is verified
    /// separately by the `failure_plugin_disappears_*` test and the supervisor
    /// unit tests in `camilladsp::supervisor`.
    #[test]
    fn failure_rust_restart_mid_stream_plugin_gets_epipe() {
        // Create a pipe manually: read-end represents CamillaDSP's stdin,
        // write-end represents the plugin's copy.
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (read_fd, write_fd) = (fds[0], fds[1]);

        // Suppress SIGPIPE so the assertion below receives EPIPE errno.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };

        // Close the read-end: simulates CamillaDSP exiting (due to Rust
        // daemon restart + supervisor cleanup killing the process).
        unsafe { libc::close(read_fd) };

        // Write to the write-end → must fail with EPIPE.
        let n = unsafe { libc::write(write_fd, b"x".as_ptr() as *const libc::c_void, 1) };
        assert_eq!(n, -1, "write to dead pipe should fail");
        let err = std::io::Error::last_os_error();
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EPIPE),
            "expected EPIPE after CamillaDSP exit, got {err}"
        );

        unsafe {
            libc::close(write_fd);
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
}
