use serde_json::Value as JsonValue;
use std::error::Error;
use std::fmt;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tungstenite::client::client_with_config;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type WsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Timeout for the initial TCP connection (three-way handshake).
///
/// Connecting to 127.0.0.1 should complete in well under a millisecond on any
/// healthy host.  Five seconds gives generous headroom while still preventing
/// an indefinite block if the port is filtered or CamillaDSP has not started
/// yet and the OS is not sending a TCP RST.
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for resolving remote hostnames to socket addresses.
///
/// Local IP literals bypass DNS entirely. For remote hostnames, this timeout
/// prevents a broken resolver path from wedging controller startup forever.
const WS_DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// TCP-level read/write timeout applied to every WebSocket operation.
///
/// 10 seconds is long enough to accommodate CamillaDSP validating a
/// configuration that includes large FIR coefficient files, while still
/// bounding a controller hang caused by a wedged or half-open socket.
const WS_IO_TIMEOUT: Duration = Duration::from_secs(10);

// ─── Error type ────────────────────────────────────────────────────────────

/// Category of a CamillaDSP application-level command error.
///
/// CamillaDSP's WebSocket protocol distinguishes several named error variants.
/// Most indicate a permanent problem with the current configuration, but some
/// are transient.  Preserving the distinction lets the controller react
/// appropriately instead of collapsing every error into a single string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandReason {
    /// Configuration failed validation or could not be read.
    /// Treat as permanent until the active config file changes.
    ConfigValidation,
    /// Request rate limit exceeded — transient, short retry.
    RateLimit,
    /// CamillaDSP is shutting down — reconnect required.
    Shutdown,
    /// The request contained an invalid value — programmer error.
    InvalidValue,
    /// Any other application-level error.
    Other,
}

impl CommandReason {
    fn from_variant(name: &str) -> Self {
        match name {
            "ConfigValidationError" | "ConfigReadError" => Self::ConfigValidation,
            "RateLimitExceededError" => Self::RateLimit,
            "ShutdownInProgressError" => Self::Shutdown,
            "InvalidValueError" => Self::InvalidValue,
            _ => Self::Other,
        }
    }
}

/// Errors that can arise when communicating with the CamillaDSP WebSocket API.
#[derive(Debug)]
pub enum WsError {
    /// The TCP/WebSocket transport failed (connect, send, read, close).
    Transport(String),
    /// CamillaDSP reported an application-level error in the reply.
    Command(CommandReason, String),
    /// The reply shape did not match the expected protocol.
    Protocol(String),
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "CamillaDSP websocket transport error: {msg}"),
            Self::Command(_, msg) => write!(f, "CamillaDSP command error: {msg}"),
            Self::Protocol(msg) => write!(f, "CamillaDSP websocket protocol error: {msg}"),
        }
    }
}

impl Error for WsError {}

// ─── Client trait ──────────────────────────────────────────────────────────

/// Abstraction over the CamillaDSP WebSocket protocol, used by the controller.
///
/// Defining a trait enables mock implementations for unit-testing the
/// controller state machine without a live CamillaDSP process.
///
/// `query` is the single low-level transport primitive: it serializes a
/// command name and optional argument, sends it, and returns the raw `value`
/// field from the reply. Every other method on this trait is a typed
/// wrapper around `query` that owns the wire-level command name, argument
/// shape, and reply parsing for one CamillaDSP API call — mirroring how
/// `pycamilladsp`'s `CamillaClient` class encapsulates the WebSocket
/// protocol behind named Python methods instead of leaking raw command
/// strings to callers. Application code (the controller, benchmarks, CLI
/// modes) should call these typed methods; `query` itself is only meant to
/// be used here and by test mocks.
pub trait CamillaClient {
    fn query(
        &mut self,
        command: &str,
        argument: Option<JsonValue>,
    ) -> Result<Option<JsonValue>, WsError>;

    /// `GetVersion` — CamillaDSP's version string.
    fn get_version(&mut self) -> Result<String, WsError> {
        let value = self.query("GetVersion", None)?;
        value
            .as_ref()
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                WsError::Protocol(format!("GetVersion returned non-string value: {value:?}"))
            })
    }

    /// `GetState` — the current `ProcessingState`.
    fn get_state(&mut self) -> Result<ProcessingState, WsError> {
        parse_processing_state(self.query("GetState", None)?)
    }

    /// `GetStopReason` — why processing last stopped (or `None`).
    fn get_stop_reason(&mut self) -> Result<StopReason, WsError> {
        parse_stop_reason(self.query("GetStopReason", None)?)
    }

    /// `GetCaptureRate` — the measured capture sample rate, or `0` if
    /// processing is not currently running.
    fn get_capture_rate(&mut self) -> Result<u64, WsError> {
        let value = self.query("GetCaptureRate", None)?;
        value.as_ref().and_then(JsonValue::as_u64).ok_or_else(|| {
            WsError::Protocol(format!(
                "GetCaptureRate returned non-numeric value: {value:?}"
            ))
        })
    }

    /// `GetConfigValue` — read a single value from the active config via a
    /// [JSON Pointer](https://datatracker.ietf.org/doc/html/rfc6901), e.g.
    /// `/devices/chunksize`.
    fn get_config_value(&mut self, pointer: &str) -> Result<Option<JsonValue>, WsError> {
        self.query(
            "GetConfigValue",
            Some(JsonValue::String(pointer.to_owned())),
        )
    }

    /// `GetConfigFilePath` — path of the config file CamillaDSP currently has
    /// loaded, if any.
    fn get_config_file_path(&mut self) -> Result<Option<String>, WsError> {
        let value = self.query("GetConfigFilePath", None)?;
        Ok(value
            .as_ref()
            .and_then(JsonValue::as_str)
            .map(str::to_owned))
    }

    /// `SetConfig` — load and start processing with `config_yaml`.
    fn set_config(&mut self, config_yaml: &str) -> Result<(), WsError> {
        self.query("SetConfig", Some(JsonValue::String(config_yaml.to_owned())))?;
        Ok(())
    }

    /// `ValidateConfig` — validate `config_yaml` without applying it.
    fn validate_config(&mut self, config_yaml: &str) -> Result<(), WsError> {
        self.query(
            "ValidateConfig",
            Some(JsonValue::String(config_yaml.to_owned())),
        )?;
        Ok(())
    }

    /// `Stop` — stop processing.
    fn stop(&mut self) -> Result<(), WsError> {
        self.query("Stop", None)?;
        Ok(())
    }
}

// ─── Client ────────────────────────────────────────────────────────────────

/// A synchronous CamillaDSP WebSocket client.
///
/// Maintains a single persistent connection for the lifetime of the controller.
/// On construction it immediately sends `GetVersion` (matching pyCamillaDSP's
/// behavior) to validate the connection before returning to the caller.
pub struct CamillaWs {
    socket: WsSocket,
}

impl CamillaWs {
    fn resolve_socket_addrs(host: &str, port: u16) -> Result<Vec<SocketAddr>, WsError> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }

        let addr_str = format!("{host}:{port}");
        resolve_socket_addrs_with_timeout(addr_str.clone(), WS_DNS_TIMEOUT, move || {
            addr_str.to_socket_addrs().map(|addrs| addrs.collect())
        })
    }

    fn connect_socket(url: &str, addr: SocketAddr) -> Result<WsSocket, WsError> {
        let tcp = TcpStream::connect_timeout(&addr, WS_CONNECT_TIMEOUT)
            .map_err(|e| WsError::Transport(format!("connect to {addr} failed: {e}")))?;

        let t = Some(WS_IO_TIMEOUT);
        tcp.set_read_timeout(t)
            .map_err(|e| WsError::Transport(format!("set_read_timeout on {addr}: {e}")))?;
        tcp.set_write_timeout(t)
            .map_err(|e| WsError::Transport(format!("set_write_timeout on {addr}: {e}")))?;

        client_with_config(url, MaybeTlsStream::Plain(tcp), None)
            .map(|(socket, _)| socket)
            .map_err(|e| WsError::Transport(format!("WebSocket handshake via {addr} failed: {e}")))
    }

    /// Connect to CamillaDSP at `ws://<host>:<port>` and issue `GetVersion`.
    ///
    /// A TCP-level read/write timeout (`WS_IO_TIMEOUT`) is set so that a
    /// wedged socket cannot block the controller thread forever.  When the
    /// timeout fires, the read/write returns an I/O error which bubbles up as
    /// `WsError::Transport`, causing the process to exit and the boot
    /// supervisor to restart it — the same clean recovery path used for any
    /// other transport failure.
    pub fn connect(host: &str, port: u16) -> Result<Self, WsError> {
        // IPv6 literals must be bracketed in URLs: ws://[::1]:1234
        let host_in_url = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let url = format!("ws://{host_in_url}:{port}");
        let addrs = Self::resolve_socket_addrs(host, port)?;

        let mut errors = Vec::new();
        let socket = addrs
            .into_iter()
            .find_map(|addr| match Self::connect_socket(url.as_str(), addr) {
                Ok(socket) => Some(socket),
                Err(err) => {
                    errors.push(err.to_string());
                    None
                }
            })
            .ok_or_else(|| {
                WsError::Transport(format!(
                    "connect failed for all resolved addresses: {}",
                    errors.join("; ")
                ))
            })?;

        let mut client = Self { socket };
        // pyCamillaDSP calls GetVersion immediately after connecting.
        client.get_version()?;
        Ok(client)
    }

    /// Send a WebSocket Close frame.
    pub fn close(&mut self) {
        let _ = self.socket.close(None);
    }
}

fn resolve_socket_addrs_with_timeout<F>(
    addr_str: String,
    timeout: Duration,
    resolver: F,
) -> Result<Vec<SocketAddr>, WsError>
where
    F: FnOnce() -> std::io::Result<Vec<SocketAddr>> + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = resolver().map_err(|err| err.to_string());
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(addrs)) if addrs.is_empty() => Err(WsError::Transport(format!(
            "address resolution returned no results: {addr_str}"
        ))),
        Ok(Ok(addrs)) => Ok(addrs),
        Ok(Err(err)) => Err(WsError::Transport(format!(
            "address resolution failed: {err}"
        ))),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(WsError::Transport(format!(
            "address resolution timed out after {}s: {addr_str}",
            timeout.as_secs()
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(WsError::Transport(format!(
            "address resolution thread terminated unexpectedly: {addr_str}"
        ))),
    }
}

impl CamillaClient for CamillaWs {
    /// Send `command` with an optional JSON argument and return the `value`
    /// field from a successful reply, or an error.
    ///
    /// The method loops over incoming frames, silently discarding Ping/Pong
    /// frames (tungstenite handles protocol-level responses automatically) and
    /// Binary/Frame frames until a Text reply or Close frame arrives.
    fn query(
        &mut self,
        command: &str,
        argument: Option<JsonValue>,
    ) -> Result<Option<JsonValue>, WsError> {
        let request = match argument {
            Some(arg) => {
                let mut obj = serde_json::Map::new();
                obj.insert(command.to_owned(), arg);
                JsonValue::Object(obj)
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
                // tungstenite queues protocol-level Pong responses automatically.
                Message::Ping(_) | Message::Pong(_) => {
                    let _ = self.socket.flush();
                }
                Message::Binary(_) | Message::Frame(_) => {}
            }
        }
    }
}

impl Drop for CamillaWs {
    fn drop(&mut self) {
        self.close();
    }
}

// ─── DeviceListener trait moved to alsa_listener.rs ───────────────────────

// ─── Reply parsing ─────────────────────────────────────────────────────────

/// Parse a CamillaDSP WebSocket reply envelope.
///
/// The envelope shape is `{"CommandName": {"result": "Ok", "value": ...}}` for
/// success, or `{"CommandName": {"result": {"ErrorVariant": "msg"}}}` for an
/// application-level error.
pub fn parse_ws_reply(command: &str, reply: JsonValue) -> Result<Option<JsonValue>, WsError> {
    let entry = reply.get(command).ok_or_else(|| {
        WsError::Protocol(format!(
            "reply does not contain command '{command}': {reply}"
        ))
    })?;

    if let Some(error) = entry.get("error") {
        return Err(WsError::Command(CommandReason::Other, error.to_string()));
    }

    let result = entry.get("result").ok_or_else(|| {
        WsError::Protocol(format!("reply for '{command}' has no result: {entry}"))
    })?;

    match result {
        JsonValue::String(value) if value == "Ok" => Ok(entry.get("value").cloned()),
        JsonValue::String(value) => Err(WsError::Command(
            CommandReason::from_variant(value),
            value.clone(),
        )),
        JsonValue::Object(values) => {
            let (kind, msg) = values
                .iter()
                .next()
                .map(|(k, v)| (k.as_str(), v.to_string()))
                .unwrap_or(("", "empty error result".to_owned()));
            Err(WsError::Command(
                CommandReason::from_variant(kind),
                format!("{kind}: {msg}"),
            ))
        }
        other => Err(WsError::Protocol(format!(
            "invalid result for '{command}': {other}"
        ))),
    }
}

// ─── Processing state ──────────────────────────────────────────────────────

/// CamillaDSP `ProcessingState` enum, matching all v4 variants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessingState {
    Running,
    Paused,
    Inactive,
    Starting,
    Stalled,
    Unknown(String),
}

/// Parse the `value` returned by `GetState` into a `ProcessingState`.
pub fn parse_processing_state(value: Option<JsonValue>) -> Result<ProcessingState, WsError> {
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

// ─── Stop reason ───────────────────────────────────────────────────────────

/// CamillaDSP `StopReason` enum, matching the v4 WebSocket JSON wire format.
#[derive(Clone, Debug, PartialEq)]
pub enum StopReason {
    None,
    Done,
    CaptureError(String),
    PlaybackError(String),
    UnknownError(String),
    /// The new sample rate is supplied by CamillaDSP as the payload.
    CaptureFormatChange(u32),
    /// The new sample rate is supplied by CamillaDSP as the payload.
    PlaybackFormatChange(u32),
    Unknown(JsonValue),
}

fn json_payload_string(value: &JsonValue) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

/// Parse the `value` returned by `GetStopReason` into a `StopReason`.
pub fn parse_stop_reason(value: Option<JsonValue>) -> Result<StopReason, WsError> {
    let value =
        value.ok_or_else(|| WsError::Protocol("GetStopReason returned no value".to_owned()))?;
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
                "CaptureFormatChange" => {
                    let rate = data
                        .as_u64()
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| {
                            WsError::Protocol(format!(
                                "CaptureFormatChange payload is not a valid u32: {data}"
                            ))
                        })?;
                    StopReason::CaptureFormatChange(rate)
                }
                "PlaybackFormatChange" => {
                    let rate = data
                        .as_u64()
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| {
                            WsError::Protocol(format!(
                                "PlaybackFormatChange payload is not a valid u32: {data}"
                            ))
                        })?;
                    StopReason::PlaybackFormatChange(rate)
                }
                _ => StopReason::Unknown(value.clone()),
            })
        }
        _ => Err(WsError::Protocol(format!(
            "unexpected GetStopReason shape: {value}"
        ))),
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Minimal scripted [`CamillaClient`] for exercising the typed
    /// convenience methods (default trait implementations) without a live
    /// CamillaDSP process.
    #[derive(Default)]
    struct MockClient {
        responses: VecDeque<Result<Option<JsonValue>, WsError>>,
        commands_sent: Vec<(String, Option<JsonValue>)>,
    }

    impl MockClient {
        fn new(responses: Vec<Result<Option<JsonValue>, WsError>>) -> Self {
            Self {
                responses: responses.into(),
                commands_sent: Vec::new(),
            }
        }
    }

    impl CamillaClient for MockClient {
        fn query(
            &mut self,
            command: &str,
            argument: Option<JsonValue>,
        ) -> Result<Option<JsonValue>, WsError> {
            self.commands_sent.push((command.to_owned(), argument));
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err(WsError::Transport("no more responses".to_owned())))
        }
    }

    #[test]
    fn get_version_sends_get_version_and_extracts_string() {
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::String("3.0.0".to_owned())))]);
        assert_eq!(client.get_version().unwrap(), "3.0.0");
        assert_eq!(client.commands_sent, vec![("GetVersion".to_owned(), None)]);
    }

    #[test]
    fn get_version_non_string_value_is_protocol_error() {
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::from(42)))]);
        assert!(matches!(client.get_version(), Err(WsError::Protocol(_))));
    }

    #[test]
    fn get_state_sends_get_state_and_parses_processing_state() {
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::String("Running".to_owned())))]);
        assert_eq!(client.get_state().unwrap(), ProcessingState::Running);
        assert_eq!(client.commands_sent, vec![("GetState".to_owned(), None)]);
    }

    #[test]
    fn get_stop_reason_sends_get_stop_reason_and_parses_reason() {
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::String("Done".to_owned())))]);
        assert_eq!(client.get_stop_reason().unwrap(), StopReason::Done);
        assert_eq!(
            client.commands_sent,
            vec![("GetStopReason".to_owned(), None)]
        );
    }

    #[test]
    fn get_capture_rate_sends_get_capture_rate_and_extracts_u64() {
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::from(48_000u64)))]);
        assert_eq!(client.get_capture_rate().unwrap(), 48_000);
        assert_eq!(
            client.commands_sent,
            vec![("GetCaptureRate".to_owned(), None)]
        );
    }

    #[test]
    fn get_capture_rate_zero_is_not_an_error() {
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::from(0u64)))]);
        assert_eq!(client.get_capture_rate().unwrap(), 0);
    }

    #[test]
    fn get_capture_rate_missing_value_is_protocol_error() {
        let mut client = MockClient::new(vec![Ok(None)]);
        assert!(matches!(
            client.get_capture_rate(),
            Err(WsError::Protocol(_))
        ));
    }

    #[test]
    fn get_config_value_sends_pointer_as_argument() {
        let mut client = MockClient::new(vec![Ok(Some(JsonValue::from(1024u64)))]);
        let value = client.get_config_value("/devices/chunksize").unwrap();
        assert_eq!(value, Some(JsonValue::from(1024u64)));
        assert_eq!(
            client.commands_sent,
            vec![(
                "GetConfigValue".to_owned(),
                Some(JsonValue::String("/devices/chunksize".to_owned()))
            )]
        );
    }

    #[test]
    fn get_config_file_path_returns_none_when_no_config_loaded() {
        let mut client = MockClient::new(vec![Ok(None)]);
        assert_eq!(client.get_config_file_path().unwrap(), None);
    }

    #[test]
    fn get_config_file_path_extracts_string() {
        let mut client =
            MockClient::new(vec![Ok(Some(JsonValue::String("/tmp/cfg.yml".to_owned())))]);
        assert_eq!(
            client.get_config_file_path().unwrap(),
            Some("/tmp/cfg.yml".to_owned())
        );
    }

    #[test]
    fn set_config_sends_config_yaml_as_string_argument() {
        let mut client = MockClient::new(vec![Ok(None)]);
        client.set_config("devices: {}").unwrap();
        assert_eq!(
            client.commands_sent,
            vec![(
                "SetConfig".to_owned(),
                Some(JsonValue::String("devices: {}".to_owned()))
            )]
        );
    }

    #[test]
    fn set_config_propagates_command_errors() {
        let mut client = MockClient::new(vec![Err(WsError::Command(
            CommandReason::ConfigValidation,
            "bad config".to_owned(),
        ))]);
        assert!(matches!(
            client.set_config("bogus"),
            Err(WsError::Command(CommandReason::ConfigValidation, _))
        ));
    }

    #[test]
    fn validate_config_sends_config_yaml_as_string_argument() {
        let mut client = MockClient::new(vec![Ok(None)]);
        client.validate_config("devices: {}").unwrap();
        assert_eq!(
            client.commands_sent,
            vec![(
                "ValidateConfig".to_owned(),
                Some(JsonValue::String("devices: {}".to_owned()))
            )]
        );
    }

    #[test]
    fn stop_sends_stop_with_no_argument() {
        let mut client = MockClient::new(vec![Ok(None)]);
        client.stop().unwrap();
        assert_eq!(client.commands_sent, vec![("Stop".to_owned(), None)]);
    }

    #[test]
    fn stop_reason_json_shapes_match_camilladsp_v4_protocol() {
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
        assert_eq!(
            parse_stop_reason(Some(serde_json::json!("Done"))).unwrap(),
            StopReason::Done
        );
    }

    #[test]
    fn stop_reason_malformed_format_change_payload_is_protocol_error() {
        assert!(matches!(
            parse_stop_reason(Some(serde_json::json!({"CaptureFormatChange": "garbage"}))),
            Err(WsError::Protocol(_))
        ));
        assert!(matches!(
            parse_stop_reason(Some(serde_json::json!({"PlaybackFormatChange": null}))),
            Err(WsError::Protocol(_))
        ));
    }

    #[test]
    fn stop_reason_multi_key_object_is_protocol_error() {
        assert!(matches!(
            parse_stop_reason(Some(
                serde_json::json!({"CaptureError": "a", "PlaybackError": "b"})
            )),
            Err(WsError::Protocol(_))
        ));
    }

    #[test]
    fn ws_reply_ok_extracts_value() {
        let reply = serde_json::json!({"GetState": {"result": "Ok", "value": "Inactive"}});
        let value = parse_ws_reply("GetState", reply).unwrap();
        assert_eq!(value, Some(serde_json::json!("Inactive")));
    }

    #[test]
    fn ws_reply_ok_without_value_returns_none() {
        let reply = serde_json::json!({"Stop": {"result": "Ok"}});
        let value = parse_ws_reply("Stop", reply).unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn ws_reply_error_result_propagates_as_command_error() {
        let err = serde_json::json!({
            "SetConfig": {
                "result": {"ConfigValidationError": "bad config"}
            }
        });
        assert!(matches!(
            parse_ws_reply("SetConfig", err),
            Err(WsError::Command(CommandReason::ConfigValidation, _))
        ));
    }

    #[test]
    fn ws_reply_string_error_propagates_as_command_error() {
        let err = serde_json::json!({"SetConfig": {"result": "Error"}});
        assert!(matches!(
            parse_ws_reply("SetConfig", err),
            Err(WsError::Command(CommandReason::Other, _))
        ));
    }

    #[test]
    fn ws_reply_rate_limit_is_classified() {
        let reply = serde_json::json!({
            "SetConfig": {
                "result": "RateLimitExceededError"
            }
        });

        assert!(matches!(
            parse_ws_reply("SetConfig", reply),
            Err(WsError::Command(CommandReason::RateLimit, _))
        ));
    }

    #[test]
    fn ws_reply_shutdown_is_classified() {
        let reply = serde_json::json!({
            "SetConfig": {
                "result": "ShutdownInProgressError"
            }
        });

        assert!(matches!(
            parse_ws_reply("SetConfig", reply),
            Err(WsError::Command(CommandReason::Shutdown, _))
        ));
    }

    #[test]
    fn resolve_socket_addrs_returns_ip_literals_without_dns() {
        let addrs = CamillaWs::resolve_socket_addrs("127.0.0.1", 1234).unwrap();
        assert_eq!(addrs, vec!["127.0.0.1:1234".parse().unwrap()]);
    }

    #[test]
    fn resolve_socket_addrs_timeout_is_reported() {
        let err = resolve_socket_addrs_with_timeout(
            "camilla.local:1234".to_owned(),
            Duration::from_millis(10),
            || {
                thread::sleep(Duration::from_millis(50));
                Ok(Vec::new())
            },
        )
        .unwrap_err();

        assert!(matches!(err, WsError::Transport(msg) if msg.contains("timed out")));
    }

    #[test]
    fn resolve_socket_addrs_empty_results_are_rejected() {
        let err = resolve_socket_addrs_with_timeout(
            "camilla.local:1234".to_owned(),
            Duration::from_millis(10),
            || Ok(Vec::new()),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            WsError::Transport(msg) if msg.contains("returned no results")
        ));
    }
}
