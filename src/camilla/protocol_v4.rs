//! CamillaDSP 4.x WebSocket protocol adapter (roadmap §24).
//!
//! Implements [`CamillaControl`] (and optionally [`CamillaStateEvents`]) against the
//! CamillaDSP 4.x wire protocol.  All JSON command names, response envelope shapes,
//! and version-specific quirks are confined to **this file** — nothing leaks to
//! `reconcile.rs`, `source/`, or `config_view.rs` (roadmap §24).
//!
//! # Wire protocol (CamillaDSP 4.x)
//!
//! Every round-trip is:
//!
//! ```text
//! → {"CommandName": <params>}          (null for no-arg commands)
//! ← {"Ok": <result>} | {"Error": <msg>}
//! ```
//!
//! State strings returned by `GetState`: `"Starting"`, `"Running"`, `"Paused"`,
//! `"Inactive"`, `"Stalled"`, `"Failed"`.
//!
//! # Connection model
//!
//! Each command opens a **fresh** WebSocket connection.  This is intentionally
//! simple and stateless: the reconciler takes a fresh snapshot on every trigger
//! (roadmap §10) and does not hold a persistent background connection.  A
//! persistent connection with multiplexed commands can be added as an optimisation
//! later, but is not architecturally necessary.
//!
//! [`CamillaStateEvents`] (state-push subscription) is implemented here for 4.2+;
//! it requires a long-lived connection and is returned as a separate type.

use std::{path::PathBuf, str::FromStr};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{
    camilla::{
        config_document::ConfigDocument,
        control::{CamillaControl, CamillaStateEvents, DspState, StopReason, Version},
    },
    error::PicorecdspError,
};

// ── V4 adapter struct ─────────────────────────────────────────────────────────

/// CamillaDSP 4.x WebSocket adapter.
///
/// `url` is the WebSocket URL, e.g. `"ws://127.0.0.1:1234"`.
pub struct CamillaDspV4 {
    url: String,
}

impl CamillaDspV4 {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    /// Open a fresh WebSocket, send one JSON command, and return the response
    /// value from the `"Ok"` envelope.  Returns `Err` if the connection fails,
    /// the response is an `"Error"` envelope, or the JSON cannot be parsed.
    async fn command(&self, msg: Value) -> Result<Value, PicorecdspError> {
        let text = serde_json::to_string(&msg).map_err(|e| {
            PicorecdspError::ProtocolError(format!("failed to serialise command: {e}"))
        })?;

        let (mut ws, _) = connect_async(&self.url)
            .await
            .map_err(|e| PicorecdspError::WebSocketOffline(format!("{e}")))?;

        ws.send(Message::Text(text))
            .await
            .map_err(|e| PicorecdspError::WebSocketOffline(format!("send failed: {e}")))?;

        let response = ws
            .next()
            .await
            .ok_or_else(|| {
                PicorecdspError::WebSocketOffline("connection closed before response".into())
            })?
            .map_err(|e| PicorecdspError::WebSocketOffline(format!("recv failed: {e}")))?;

        let _ = ws.close(None).await; // best-effort close

        let text = match response {
            Message::Text(t) => t,
            other => {
                return Err(PicorecdspError::ProtocolError(format!(
                    "expected text frame, got {other:?}"
                )))
            }
        };

        let envelope: Value = serde_json::from_str(&text).map_err(|e| {
            PicorecdspError::ProtocolError(format!("invalid JSON in response: {e} (raw: {text})"))
        })?;

        match envelope.get("Ok") {
            Some(v) => Ok(v.clone()),
            None => {
                let err_msg = envelope
                    .get("Error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned();
                Err(PicorecdspError::ProtocolError(err_msg))
            }
        }
    }
}

// ── Wire-format helpers ───────────────────────────────────────────────────────

/// Parse a `"major.minor.patch"` version string from a `Value::String`.
fn parse_version(v: &Value) -> Result<Version, PicorecdspError> {
    let s = v.as_str().ok_or_else(|| {
        PicorecdspError::ProtocolError("GetVersion result is not a string".into())
    })?;
    s.parse::<Version>().map_err(PicorecdspError::ProtocolError)
}

/// Parse a DSP state string as returned by `GetState`.
fn parse_state(v: &Value) -> Result<DspState, PicorecdspError> {
    match v.as_str() {
        Some("Starting") => Ok(DspState::Starting),
        Some("Running") => Ok(DspState::Running),
        Some("Paused") => Ok(DspState::Paused),
        Some("Inactive") | Some("Idle") | Some("Stopped") => Ok(DspState::Inactive),
        Some("Stalled") => Ok(DspState::Stalled),
        Some("Failed") | Some("Error") => Ok(DspState::Failed),
        other => Err(PicorecdspError::ProtocolError(format!(
            "unknown GetState value: {other:?}"
        ))),
    }
}

/// Parse an optional stop-reason string as returned by `GetStopReason`.
fn parse_stop_reason(v: &Value) -> StopReason {
    match v.as_str() {
        None | Some("None") | Some("") => StopReason::None,
        Some("CaptureError") => StopReason::CaptureError,
        Some("PlaybackError") => StopReason::PlaybackError,
        Some("CaptureFormatChange") => StopReason::CaptureFormatChange,
        Some("PlaybackFormatChange") => StopReason::PlaybackFormatChange,
        Some("Done") => StopReason::Done,
        Some(other) => StopReason::Other(other.to_owned()),
    }
}

/// Parse an optional config YAML string.  `CamillaDSP` returns `null` (or
/// `"Error": ...`) when no config is active/previous.
fn parse_optional_config(v: &Value) -> Result<Option<ConfigDocument>, PicorecdspError> {
    match v {
        Value::Null => Ok(None),
        Value::String(yaml) if yaml.trim().is_empty() => Ok(None),
        Value::String(yaml) => {
            let doc = ConfigDocument::from_yaml(yaml)
                .map_err(|e| PicorecdspError::ConfigRead(e.to_string()))?;
            Ok(Some(doc))
        }
        other => Err(PicorecdspError::ProtocolError(format!(
            "expected string YAML or null, got {other:?}"
        ))),
    }
}

// ── CamillaControl implementation ─────────────────────────────────────────────

#[async_trait]
impl CamillaControl for CamillaDspV4 {
    async fn version(&self) -> Result<Version, PicorecdspError> {
        let v = self.command(json!({"GetVersion": null})).await?;
        parse_version(&v)
    }

    async fn state(&self) -> Result<DspState, PicorecdspError> {
        let v = self.command(json!({"GetState": null})).await?;
        parse_state(&v)
    }

    async fn stop_reason(&self) -> Result<Option<StopReason>, PicorecdspError> {
        // GetStopReason was added in 4.2; on older versions the WebSocket returns
        // an Error envelope.  We map that to Ok(None) rather than propagating an
        // error, because a missing stop reason is not fatal to the reconciler.
        match self.command(json!({"GetStopReason": null})).await {
            Ok(v) => Ok(Some(parse_stop_reason(&v))),
            Err(PicorecdspError::ProtocolError(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn active_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
        match self.command(json!({"GetConfig": null})).await {
            Ok(v) => parse_optional_config(&v),
            // CamillaDSP returns an Error envelope when there is no active config
            // (e.g. started with --no_config and no SetConfig yet).
            Err(PicorecdspError::ProtocolError(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn previous_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
        match self.command(json!({"GetPreviousConfig": null})).await {
            Ok(v) => parse_optional_config(&v),
            Err(PicorecdspError::ProtocolError(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn config_file_path(&self) -> Result<Option<PathBuf>, PicorecdspError> {
        match self.command(json!({"GetConfigFilePath": null})).await {
            Ok(Value::Null) => Ok(None),
            Ok(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Ok(Value::String(s)) => Ok(Some(PathBuf::from_str(&s).unwrap())),
            Ok(other) => Err(PicorecdspError::ProtocolError(format!(
                "GetConfigFilePath: unexpected value {other:?}"
            ))),
            Err(PicorecdspError::ProtocolError(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn set_config(&self, config: &ConfigDocument) -> Result<(), PicorecdspError> {
        let yaml = config
            .to_yaml()
            .map_err(|e| PicorecdspError::ConfigRead(e.to_string()))?;
        self.command(json!({"SetConfig": yaml})).await?;
        Ok(())
    }

    async fn set_config_value(&self, path: &str, value: Value) -> Result<(), PicorecdspError> {
        self.command(json!({
            "SetConfigValue": {
                "param": path,
                "value": value
            }
        }))
        .await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), PicorecdspError> {
        self.command(json!({"Stop": null})).await?;
        Ok(())
    }
}

// ── CamillaStateEvents (4.2+) ─────────────────────────────────────────────────

/// State-event subscription for CamillaDSP 4.2+ (roadmap §22 / Cliffhanger E).
///
/// **Removal criterion (Cliffhanger E, upstream/capabilities.yml):** once the
/// production baseline reliably supports `SubscribeState`, the fast-polling
/// fallback path in `DspTriggerSource` can be deleted.  This subscriber stays.
pub struct CamillaDspV4StateEvents {
    url: String,
}

impl CamillaDspV4StateEvents {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[async_trait]
impl CamillaStateEvents for CamillaDspV4StateEvents {
    async fn subscribe_state(
        &self,
    ) -> Result<tokio::sync::mpsc::Receiver<DspState>, PicorecdspError> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let url = self.url.clone();

        tokio::spawn(async move {
            let result: Result<(), String> = async {
                let (mut ws, _) = connect_async(&url).await.map_err(|e| e.to_string())?;

                // Send SubscribeState command.
                let cmd = serde_json::to_string(&json!({"SubscribeState": null}))
                    .map_err(|e| e.to_string())?;
                ws.send(Message::Text(cmd))
                    .await
                    .map_err(|e| e.to_string())?;

                while let Some(msg) = ws.next().await {
                    let msg = msg.map_err(|e| e.to_string())?;
                    let text = match msg {
                        Message::Text(t) => t,
                        Message::Close(_) => break,
                        _ => continue,
                    };
                    let envelope: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if let Some(state_val) = envelope.get("State") {
                        if let Ok(state) = parse_state(state_val) {
                            if tx.send(state).await.is_err() {
                                break; // receiver dropped
                            }
                        }
                    }
                }
                Ok(())
            }
            .await;

            if let Err(e) = result {
                log::warn!("CamillaDSP v4 state subscription ended: {e}");
            }
        });

        Ok(rx)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_state_all_variants() {
        for (s, expected) in &[
            ("Starting", DspState::Starting),
            ("Running", DspState::Running),
            ("Paused", DspState::Paused),
            ("Inactive", DspState::Inactive),
            ("Idle", DspState::Inactive),
            ("Stopped", DspState::Inactive),
            ("Stalled", DspState::Stalled),
            ("Failed", DspState::Failed),
            ("Error", DspState::Failed),
        ] {
            assert_eq!(
                parse_state(&Value::String((*s).to_owned())).unwrap(),
                *expected,
                "state string `{s}`"
            );
        }
    }

    #[test]
    fn parse_state_unknown_is_error() {
        assert!(parse_state(&Value::String("XYZ".into())).is_err());
    }

    #[test]
    fn parse_stop_reason_variants() {
        assert_eq!(parse_stop_reason(&Value::Null), StopReason::None);
        assert_eq!(
            parse_stop_reason(&Value::String("CaptureError".into())),
            StopReason::CaptureError
        );
        assert_eq!(
            parse_stop_reason(&Value::String("PlaybackError".into())),
            StopReason::PlaybackError
        );
        assert_eq!(
            parse_stop_reason(&Value::String("Done".into())),
            StopReason::Done
        );
        assert!(matches!(
            parse_stop_reason(&Value::String("SomeNewReason".into())),
            StopReason::Other(_)
        ));
    }

    #[test]
    fn parse_optional_config_null_gives_none() {
        assert!(parse_optional_config(&Value::Null).unwrap().is_none());
    }

    #[test]
    fn parse_optional_config_empty_string_gives_none() {
        assert!(parse_optional_config(&Value::String("".into()))
            .unwrap()
            .is_none());
    }

    #[test]
    fn parse_optional_config_valid_yaml_gives_document() {
        let yaml = r#"
devices:
  samplerate: 44100
  capture:
    type: Alsa
    device: "hw:Loopback,0,0"
    channels: 2
    format: S32_LE
    stop_on_inactive: true
"#;
        let doc = parse_optional_config(&Value::String(yaml.to_owned()))
            .unwrap()
            .expect("must be Some");
        assert_eq!(
            doc.get("devices.samplerate"),
            Some(&Value::Number(44100.into()))
        );
    }
}
