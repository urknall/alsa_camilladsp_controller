//! Logging initialisation for the piCoreCDSP v2 binary (roadmap §36).
//!
//! This module configures levelled logging for the controller process. It is
//! called once at startup from `main.rs` and is intentionally kept thin:
//! reconciler, source-observer, and CamillaControl adapter code emits log
//! records through the standard `log` facade; this module decides *how* those
//! records are formatted and where they go.
//!
//! # Design constraints
//!
//! * Logging must not be coupled to any particular deployment target.
//!   `RUST_LOG` / `PICORECDSP_LOG` controls the level at runtime.
//! * Structured fields (source rate, DSP state, reconcile action) must be
//!   preserved so that log-scraping on pCP can feed a simple status display.
//! * Failures inside the logging setup must never crash the controller — a
//!   fallback to `stderr` is always acceptable.
//!
//! # Current implementation
//!
//! The binary does not yet have a rich logging back-end (no `env_logger` or
//! `tracing-subscriber` dependency has been added). This module provides the
//! module boundary and the startup hook so that a back-end can be wired in
//! without touching any other source file.

use std::env;

/// Log levels recognised by [`init_logging`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl std::str::FromStr for LogLevel {
    /// Parsing is infallible: unknown strings map to `LogLevel::Info`.
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "error" => LogLevel::Error,
            "warn" | "warning" => LogLevel::Warn,
            "debug" => LogLevel::Debug,
            "trace" => LogLevel::Trace,
            _ => LogLevel::Info,
        })
    }
}

/// Determine the configured log level from the environment.
///
/// Reads `PICORECDSP_LOG` first, then falls back to `RUST_LOG`, then to
/// `"info"`.
pub fn configured_level() -> LogLevel {
    let raw = env::var("PICORECDSP_LOG")
        .or_else(|_| env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".into());
    raw.parse().unwrap()
}

/// Initialise logging for the piCoreCDSP controller process.
///
/// In the current implementation this is a lightweight placeholder: it reads
/// the configured level and emits a startup line to `stderr`. When a
/// structured logging back-end (e.g. `env_logger` or `tracing-subscriber`) is
/// added as a dependency, this function should be the only call site that
/// changes.
///
/// # Idempotency
///
/// Safe to call more than once; subsequent calls are no-ops once a back-end
/// has been installed.
pub fn init_logging() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let level = configured_level();
        eprintln!("piCoreCDSP: logging initialised (level: {level:?})");
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_parse_known_strings() {
        assert_eq!("info".parse::<LogLevel>(), Ok(LogLevel::Info));
        assert_eq!("INFO".parse::<LogLevel>(), Ok(LogLevel::Info));
        assert_eq!("debug".parse::<LogLevel>(), Ok(LogLevel::Debug));
        assert_eq!("warn".parse::<LogLevel>(), Ok(LogLevel::Warn));
        assert_eq!("warning".parse::<LogLevel>(), Ok(LogLevel::Warn));
        assert_eq!("error".parse::<LogLevel>(), Ok(LogLevel::Error));
        assert_eq!("trace".parse::<LogLevel>(), Ok(LogLevel::Trace));
    }

    #[test]
    fn level_parse_unknown_defaults_to_info() {
        assert_eq!("verbose".parse::<LogLevel>(), Ok(LogLevel::Info));
        assert_eq!("".parse::<LogLevel>(), Ok(LogLevel::Info));
    }

    #[test]
    fn init_logging_is_idempotent() {
        // Should not panic even when called multiple times.
        init_logging();
        init_logging();
    }
}
