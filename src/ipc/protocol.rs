//! Versioned IPC message protocol for the piCoreDSP ioplug plugin ↔ Rust
//! controller channel.
//!
//! Design requirements covered here:
//! - Transport-independent framing for AF_UNIX byte streams.
//! - Explicit protocol version field from day one.
//! - Fixed little-endian binary wire format.
//! - Unknown-message rejection.
//! - Maximum message length guardrail.
//! - Negotiation helper for reconnect/compatibility scenarios.

use std::fmt;
use std::io;

/// Current protocol version implemented by the controller.
pub const PROTOCOL_VERSION: u8 = 1;
/// Lowest version this controller still accepts.
pub const PROTOCOL_VERSION_MIN: u8 = 1;
/// Hard upper bound for one message frame in bytes.
pub const MAX_MESSAGE_LEN: usize = 64;

/// Wire message type tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Hello = 0x01,
    Start = 0x02,
    Stop = 0x03,
    Ready = 0x04,
    Error = 0x05,
}

impl MessageType {
    pub fn from_u8(value: u8) -> Result<Self, ProtocolError> {
        match value {
            0x01 => Ok(Self::Hello),
            0x02 => Ok(Self::Start),
            0x03 => Ok(Self::Stop),
            0x04 => Ok(Self::Ready),
            0x05 => Ok(Self::Error),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }

    fn expected_len(self) -> usize {
        match self {
            Self::Hello => 2,
            Self::Start => 8,
            Self::Stop => 2,
            Self::Ready => 2,
            Self::Error => 3,
        }
    }
}

/// Error codes returned by controller `ERROR` messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    Config = 1,
    PlaybackDevice = 2,
    Protocol = 3,
    Internal = 4,
}

impl ErrorCode {
    fn from_u8(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Config),
            2 => Ok(Self::PlaybackDevice),
            3 => Ok(Self::Protocol),
            4 => Ok(Self::Internal),
            other => Err(ProtocolError::UnknownErrorCode(other)),
        }
    }
}

/// Plugin ↔ controller messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginMessage {
    Hello {
        version: u8,
    },
    Start {
        version: u8,
        rate: u32,
        format: u8,
        channels: u8,
    },
    Stop {
        version: u8,
    },
    Ready {
        version: u8,
    },
    Error {
        version: u8,
        code: ErrorCode,
    },
}

impl PluginMessage {
    pub fn message_type(&self) -> MessageType {
        match self {
            Self::Hello { .. } => MessageType::Hello,
            Self::Start { .. } => MessageType::Start,
            Self::Stop { .. } => MessageType::Stop,
            Self::Ready { .. } => MessageType::Ready,
            Self::Error { .. } => MessageType::Error,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Hello { version } => vec![MessageType::Hello as u8, *version],
            Self::Start {
                version,
                rate,
                format,
                channels,
            } => {
                let mut out = Vec::with_capacity(8);
                out.push(MessageType::Start as u8);
                out.push(*version);
                out.extend_from_slice(&rate.to_le_bytes());
                out.push(*format);
                out.push(*channels);
                out
            }
            Self::Stop { version } => vec![MessageType::Stop as u8, *version],
            Self::Ready { version } => vec![MessageType::Ready as u8, *version],
            Self::Error { version, code } => vec![MessageType::Error as u8, *version, *code as u8],
        }
    }

    pub fn decode(frame: &[u8]) -> Result<Self, ProtocolError> {
        if frame.len() < 2 {
            return Err(ProtocolError::TruncatedFrame {
                expected_at_least: 2,
                actual: frame.len(),
            });
        }
        if frame.len() > MAX_MESSAGE_LEN {
            return Err(ProtocolError::FrameTooLong {
                max: MAX_MESSAGE_LEN,
                actual: frame.len(),
            });
        }

        let ty = MessageType::from_u8(frame[0])?;
        let expected = ty.expected_len();
        if frame.len() != expected {
            return Err(ProtocolError::InvalidLength {
                message_type: ty,
                expected,
                actual: frame.len(),
            });
        }

        let version = frame[1];
        let msg = match ty {
            MessageType::Hello => Self::Hello { version },
            MessageType::Start => {
                let rate = u32::from_le_bytes([frame[2], frame[3], frame[4], frame[5]]);
                Self::Start {
                    version,
                    rate,
                    format: frame[6],
                    channels: frame[7],
                }
            }
            MessageType::Stop => Self::Stop { version },
            MessageType::Ready => Self::Ready { version },
            MessageType::Error => Self::Error {
                version,
                code: ErrorCode::from_u8(frame[2])?,
            },
        };

        Ok(msg)
    }
}

/// Negotiate the protocol version.
///
/// The negotiated version is `min(plugin_version, controller_version)` and must
/// be at least [`PROTOCOL_VERSION_MIN`].
pub fn negotiate_version(plugin_version: u8, controller_version: u8) -> Result<u8, ProtocolError> {
    let negotiated = plugin_version.min(controller_version);
    if negotiated < PROTOCOL_VERSION_MIN {
        return Err(ProtocolError::UnsupportedVersion {
            requested: plugin_version,
            minimum_supported: PROTOCOL_VERSION_MIN,
        });
    }
    Ok(negotiated)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    UnknownMessageType(u8),
    UnexpectedMessageType {
        expected: MessageType,
        actual: MessageType,
    },
    UnknownErrorCode(u8),
    InvalidLength {
        message_type: MessageType,
        expected: usize,
        actual: usize,
    },
    TruncatedFrame {
        expected_at_least: usize,
        actual: usize,
    },
    FrameTooLong {
        max: usize,
        actual: usize,
    },
    UnsupportedVersion {
        requested: u8,
        minimum_supported: u8,
    },
    HandshakeNotComplete,
    Timeout,
    Disconnected,
    Io(String),
}

impl From<io::Error> for ProtocolError {
    fn from(value: io::Error) -> Self {
        if matches!(
            value.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            return Self::Timeout;
        }
        if value.kind() == io::ErrorKind::UnexpectedEof {
            return Self::Disconnected;
        }
        Self::Io(value.to_string())
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMessageType(v) => write!(f, "unknown IPC message type: 0x{v:02x}"),
            Self::UnexpectedMessageType { expected, actual } => write!(
                f,
                "unexpected IPC message type: expected {expected:?}, got {actual:?}"
            ),
            Self::UnknownErrorCode(v) => write!(f, "unknown IPC error code: {v}"),
            Self::InvalidLength {
                message_type,
                expected,
                actual,
            } => write!(
                f,
                "invalid frame length for {message_type:?}: expected {expected}, got {actual}"
            ),
            Self::TruncatedFrame {
                expected_at_least,
                actual,
            } => write!(
                f,
                "truncated frame: expected at least {expected_at_least} bytes, got {actual}"
            ),
            Self::FrameTooLong { max, actual } => {
                write!(f, "frame too long: max {max} bytes, got {actual}")
            }
            Self::UnsupportedVersion {
                requested,
                minimum_supported,
            } => write!(
                f,
                "unsupported protocol version {requested}, minimum supported is {minimum_supported}"
            ),
            Self::HandshakeNotComplete => write!(f, "IPC handshake not completed"),
            Self::Timeout => write!(f, "IPC operation timed out"),
            Self::Disconnected => write!(f, "IPC peer disconnected"),
            Self::Io(err) => write!(f, "IPC I/O error: {err}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Returns expected frame length for known message types.
pub fn expected_frame_len(type_byte: u8) -> Result<usize, ProtocolError> {
    let msg_type = MessageType::from_u8(type_byte)?;
    Ok(msg_type.expected_len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_roundtrip_uses_little_endian_rate() {
        let msg = PluginMessage::Start {
            version: 1,
            rate: 96_000,
            format: 6,
            channels: 2,
        };
        let encoded = msg.encode();
        assert_eq!(&encoded[2..6], &96_000u32.to_le_bytes());
        assert_eq!(PluginMessage::decode(&encoded).unwrap(), msg);
    }

    #[test]
    fn encode_decode_other_message_variants() {
        let cases = [
            PluginMessage::Hello { version: 1 },
            PluginMessage::Stop { version: 1 },
            PluginMessage::Ready { version: 1 },
            PluginMessage::Error {
                version: 1,
                code: ErrorCode::Config,
            },
        ];

        for case in cases {
            let encoded = case.encode();
            assert_eq!(PluginMessage::decode(&encoded).unwrap(), case);
        }
    }

    #[test]
    fn unknown_message_type_is_rejected() {
        let err = PluginMessage::decode(&[0xff, 1]).unwrap_err();
        assert!(matches!(err, ProtocolError::UnknownMessageType(0xff)));
    }

    #[test]
    fn invalid_length_is_rejected() {
        let err = PluginMessage::decode(&[MessageType::Start as u8, 1, 0]).unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidLength { .. }));
    }

    #[test]
    fn negotiate_version_picks_lower_and_checks_minimum() {
        assert_eq!(negotiate_version(1, 1).unwrap(), 1);
        assert_eq!(negotiate_version(1, 2).unwrap(), 1);
        let err = negotiate_version(0, 1).unwrap_err();
        assert!(matches!(err, ProtocolError::UnsupportedVersion { .. }));
    }
}
