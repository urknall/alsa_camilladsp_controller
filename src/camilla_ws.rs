use crate::error::app_error;
use serde_json::Value as JsonValue;
use std::error::Error;
use std::fmt;
use std::net::TcpStream;
use tungstenite::client::connect;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type WsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

// ─── Error type ────────────────────────────────────────────────────────────

/// Errors that can arise when communicating with the CamillaDSP WebSocket API.
#[derive(Debug)]
pub enum WsError {
    /// The TCP/WebSocket transport failed (connect, send, read, close).
    Transport(String),
    /// CamillaDSP reported an application-level error in the reply.
    Command(String),
    /// The reply shape did not match the expected protocol.
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
    /// Connect to CamillaDSP at `ws://<host>:<port>` and issue `GetVersion`.
    pub fn connect(host: &str, port: u16) -> Result<Self, WsError> {
        let url = format!("ws://{host}:{port}");
        let (socket, _) = connect(url)
            .map_err(|err| WsError::Transport(format!("connect failed: {err}")))?;
        let mut client = Self { socket };
        // pyCamillaDSP calls GetVersion immediately after connecting.
        let _ = client.query("GetVersion", None)?;
        Ok(client)
    }

    /// Send `command` with an optional JSON argument and return the `value`
    /// field from a successful reply, or an error.
    ///
    /// The method loops over incoming frames, silently discarding Ping/Pong
    /// frames (tungstenite handles protocol-level responses automatically) and
    /// Binary/Frame frames until a Text reply or Close frame arrives.
    pub fn query(
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

    /// Send a WebSocket Close frame.
    pub fn close(&mut self) {
        let _ = self.socket.close(None);
    }
}

impl Drop for CamillaWs {
    fn drop(&mut self) {
        self.close();
    }
}

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
        return Err(WsError::Command(error.to_string()));
    }

    let result = entry
        .get("result")
        .ok_or_else(|| WsError::Protocol(format!("reply for '{command}' has no result: {entry}")))?;

    match result {
        JsonValue::String(value) if value == "Ok" => Ok(entry.get("value").cloned()),
        JsonValue::String(value) => Err(WsError::Command(value.clone())),
        JsonValue::Object(values) => {
            let message = values
                .iter()
                .next()
                .map(|(kind, msg)| format!("{kind}: {msg}"))
                .unwrap_or_else(|| "empty error result".to_owned());
            Err(WsError::Command(message))
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
    let value =
        value.ok_or_else(|| WsError::Protocol("GetState returned no value".to_owned()))?;
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

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
            Err(WsError::Command(_))
        ));
    }

    #[test]
    fn ws_reply_string_error_propagates_as_command_error() {
        let err = serde_json::json!({"SetConfig": {"result": "Error"}});
        assert!(matches!(
            parse_ws_reply("SetConfig", err),
            Err(WsError::Command(_))
        ));
    }
}
