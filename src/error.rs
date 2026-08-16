//! Error types for piCoreCDSP v2 (roadmap §33).
//!
//! All error conditions the reconciler can encounter are represented here:
//! WebSocket offline, protocol errors, config read failures, transport contract
//! violations, source observer failures, and the `$samplerate$` token guard.

use crate::camilla::TransportContractViolation;

/// Top-level error type for all piCoreCDSP operations.
#[derive(Debug)]
pub enum PicorecdspError {
    /// The CamillaDSP WebSocket is not reachable (roadmap §33: WebSocket offline
    /// recovery — bounded backoff, reconnect, full fresh snapshot).
    WebSocketOffline(String),

    /// A message was received from CamillaDSP that did not match the expected
    /// wire protocol for this version.
    ProtocolError(String),

    /// The CamillaDSP config could not be read at the known paths (e.g. the YAML
    /// was structurally valid but the `devices.*` section was missing or malformed).
    ConfigRead(String),

    /// The transport contract (roadmap §7) is violated — managed mode is suspended
    /// and the user must fix the config. Rust never repairs it.
    TransportContract(Vec<TransportContractViolation>),

    /// The `snd-aloop` source observer encountered an error (HCTL open/read/poll).
    SourceObserver(String),

    /// A `$samplerate$`-materialized resource was detected in the active config.
    /// piCoreCDSP cannot safely patch the rate field in this case (roadmap §21).
    /// The user must use a fixed DSP rate + resampler or separate configs as an
    /// alternative.
    SamplerateTokenGuard {
        /// The config path or string value that contains the token.
        detail: String,
    },

    /// The reconciler attempted to write a config value but CamillaDSP rejected it
    /// because the process is in a transitional state.  The caller should retry
    /// after a fresh snapshot.
    RateLimitExceeded,

    /// I/O error (file, socket, etc.).
    Io(std::io::Error),
}

impl std::fmt::Display for PicorecdspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PicorecdspError::WebSocketOffline(msg) => {
                write!(f, "CamillaDSP WebSocket offline: {msg}")
            }
            PicorecdspError::ProtocolError(msg) => {
                write!(f, "CamillaDSP protocol error: {msg}")
            }
            PicorecdspError::ConfigRead(msg) => {
                write!(f, "failed to read CamillaDSP config: {msg}")
            }
            PicorecdspError::TransportContract(violations) => {
                let detail = violations
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                write!(f, "transport contract violated: {detail}")
            }
            PicorecdspError::SourceObserver(msg) => {
                write!(f, "snd-aloop source observer error: {msg}")
            }
            PicorecdspError::SamplerateTokenGuard { detail } => {
                write!(
                    f,
                    "$samplerate$ token detected in active config — cannot safely \
                     patch rate field; use a fixed DSP rate + resampler or separate \
                     per-rate configs as alternatives. Detail: {detail}"
                )
            }
            PicorecdspError::RateLimitExceeded => {
                write!(f, "CamillaDSP rejected config write (transitional state); retry after fresh snapshot")
            }
            PicorecdspError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for PicorecdspError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PicorecdspError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PicorecdspError {
    fn from(e: std::io::Error) -> Self {
        PicorecdspError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camilla::TransportContractViolation;

    #[test]
    fn websocket_offline_displays_clearly() {
        let err = PicorecdspError::WebSocketOffline("connection refused".into());
        assert!(err.to_string().contains("WebSocket offline"));
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn transport_contract_displays_all_violations() {
        let err = PicorecdspError::TransportContract(vec![
            TransportContractViolation::StopOnInactiveDisabled,
            TransportContractViolation::UnexpectedChannels {
                expected: 2,
                actual: 1,
            },
        ]);
        let msg = err.to_string();
        assert!(msg.contains("stop_on_inactive"));
        assert!(msg.contains("channels"));
    }

    #[test]
    fn samplerate_token_guard_is_actionable() {
        let err = PicorecdspError::SamplerateTokenGuard {
            detail: "filters.EQ.parameters.filename = fir_$samplerate$.wav".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("$samplerate$"));
        assert!(msg.contains("resampler"));
    }

    #[test]
    fn io_error_wraps_and_sources_correctly() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err = PicorecdspError::from(io);
        assert!(err.to_string().contains("no such file"));
        assert!(std::error::Error::source(&err).is_some());
    }
}
