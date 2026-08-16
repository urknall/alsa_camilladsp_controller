//! CamillaDSP 5.x canary WebSocket protocol adapter (roadmap §24).
//!
//! This adapter implements [`CamillaControl`] against the CamillaDSP 5.x wire
//! protocol.  It is a **canary** during the v2 development cycle: both v4 and v5
//! adapters implement the same semantic API, and the reconciler is tested against
//! both in parallel (roadmap §24).
//!
//! # Wire protocol differences from 4.x
//!
//! CamillaDSP 5.x (`next5` branch as of the v2 development snapshot) keeps a
//! similar request/response envelope but renames several state strings and adds
//! the `SubscribeState` push event.  Known differences:
//!
//! * `"Idle"` → `"Inactive"` (5.x uses `"Inactive"` as the canonical inactive
//!   name; 4.x may return `"Idle"` on some versions).
//! * `SubscribeState` is available and is the preferred state-change notification
//!   path.
//! * `SetConfigValue` uses the same dotted path format.
//!
//! This file tracks the `next5` WebSocket API.  Any deviation from the 4.x
//! behaviour that is confirmed against the actual `next5` source must be updated
//! here — **do not make assumptions; verify against the upstream source**.
//!
//! # Deletion plan
//!
//! Before the v2 production release exactly one adapter will be active.
//! After the final v5 upgrade: `protocol_v4.rs → DELETE`.
//! See `upstream/capabilities.yml` entry `camilla_v5_wire_format`.

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

// ── V5 adapter struct ─────────────────────────────────────────────────────────

/// CamillaDSP 5.x (next5) WebSocket adapter.
///
/// Wire protocol is identical to v4 for simple commands; differences are noted
/// inline.
pub struct CamillaDspV5 {
    url: String,
}

impl CamillaDspV5 {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

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

        let _ = ws.close(None).await;

        let text = match response {
            Message::Text(t) => t,
            other => {
                return Err(PicorecdspError::ProtocolError(format!(
                    "expected text frame, got {other:?}"
                )))
            }
        };

        let envelope: Value = serde_json::from_str(&text).map_err(|e| {
            PicorecdspError::ProtocolError(format!("invalid JSON: {e} (raw: {text})"))
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

// ── Wire-format helpers (v5) ──────────────────────────────────────────────────

fn parse_version_v5(v: &Value) -> Result<Version, PicorecdspError> {
    let s = v.as_str().ok_or_else(|| {
        PicorecdspError::ProtocolError("GetVersion result is not a string".into())
    })?;
    s.parse::<Version>().map_err(PicorecdspError::ProtocolError)
}

/// CamillaDSP 5.x state string mapping.  `"Inactive"` is the canonical name in
/// 5.x; `"Idle"` and `"Stopped"` are accepted as aliases for forward-compat with
/// builds that haven't fully migrated the string yet.
fn parse_state_v5(v: &Value) -> Result<DspState, PicorecdspError> {
    match v.as_str() {
        Some("Starting") => Ok(DspState::Starting),
        Some("Running") => Ok(DspState::Running),
        Some("Paused") => Ok(DspState::Paused),
        Some("Inactive") | Some("Idle") | Some("Stopped") => Ok(DspState::Inactive),
        Some("Stalled") => Ok(DspState::Stalled),
        Some("Failed") | Some("Error") => Ok(DspState::Failed),
        other => Err(PicorecdspError::ProtocolError(format!(
            "unknown GetState value (v5): {other:?}"
        ))),
    }
}

fn parse_stop_reason_v5(v: &Value) -> StopReason {
    // Same mapping as v4 for now — update if 5.x renames any reason string.
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

fn parse_optional_config_v5(v: &Value) -> Result<Option<ConfigDocument>, PicorecdspError> {
    match v {
        Value::Null => Ok(None),
        Value::String(yaml) if yaml.trim().is_empty() => Ok(None),
        Value::String(yaml) => {
            let doc = ConfigDocument::from_yaml(yaml)
                .map_err(|e| PicorecdspError::ConfigRead(e.to_string()))?;
            Ok(Some(doc))
        }
        other => Err(PicorecdspError::ProtocolError(format!(
            "expected string YAML or null (v5), got {other:?}"
        ))),
    }
}

// ── CamillaControl implementation ─────────────────────────────────────────────

#[async_trait]
impl CamillaControl for CamillaDspV5 {
    async fn version(&self) -> Result<Version, PicorecdspError> {
        let v = self.command(json!({"GetVersion": null})).await?;
        parse_version_v5(&v)
    }

    async fn state(&self) -> Result<DspState, PicorecdspError> {
        let v = self.command(json!({"GetState": null})).await?;
        parse_state_v5(&v)
    }

    async fn stop_reason(&self) -> Result<Option<StopReason>, PicorecdspError> {
        match self.command(json!({"GetStopReason": null})).await {
            Ok(v) => Ok(Some(parse_stop_reason_v5(&v))),
            Err(PicorecdspError::ProtocolError(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn active_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
        match self.command(json!({"GetConfig": null})).await {
            Ok(v) => parse_optional_config_v5(&v),
            Err(PicorecdspError::ProtocolError(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn previous_config(&self) -> Result<Option<ConfigDocument>, PicorecdspError> {
        match self.command(json!({"GetPreviousConfig": null})).await {
            Ok(v) => parse_optional_config_v5(&v),
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
                "GetConfigFilePath (v5): unexpected value {other:?}"
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

// ── CamillaStateEvents (5.x — SubscribeState always available) ────────────────

/// State-event subscription using CamillaDSP 5.x `SubscribeState`.
///
/// In 5.x `SubscribeState` is always available (unlike 4.x where it requires
/// 4.2+).  This subscriber holds a long-lived WebSocket connection and forwards
/// state-change events to the reconciler.
pub struct CamillaDspV5StateEvents {
    url: String,
}

impl CamillaDspV5StateEvents {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[async_trait]
impl CamillaStateEvents for CamillaDspV5StateEvents {
    async fn subscribe_state(
        &self,
    ) -> Result<tokio::sync::mpsc::Receiver<DspState>, PicorecdspError> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let url = self.url.clone();

        tokio::spawn(async move {
            let result: Result<(), String> = async {
                let (mut ws, _) = connect_async(&url).await.map_err(|e| e.to_string())?;

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
                        if let Ok(state) = parse_state_v5(state_val) {
                            if tx.send(state).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                Ok(())
            }
            .await;

            if let Err(e) = result {
                log::warn!("CamillaDSP v5 state subscription ended: {e}");
            }
        });

        Ok(rx)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_state_v5_inactive_variants() {
        for s in &["Inactive", "Idle", "Stopped"] {
            assert_eq!(
                parse_state_v5(&Value::String((*s).to_owned())).unwrap(),
                DspState::Inactive,
                "state string `{s}`"
            );
        }
    }

    #[test]
    fn parse_state_v5_active_variants() {
        assert_eq!(
            parse_state_v5(&Value::String("Running".into())).unwrap(),
            DspState::Running
        );
        assert_eq!(
            parse_state_v5(&Value::String("Paused".into())).unwrap(),
            DspState::Paused
        );
    }

    #[test]
    fn parse_stop_reason_v5_none() {
        assert_eq!(parse_stop_reason_v5(&Value::Null), StopReason::None);
        assert_eq!(
            parse_stop_reason_v5(&Value::String("None".into())),
            StopReason::None
        );
    }
}
