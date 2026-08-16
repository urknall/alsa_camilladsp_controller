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
//! # Implementation
//!
//! `env_logger` writes timestamped, levelled lines to `stderr`, which is
//! captured by pCP's syslog.  `PICORECDSP_LOG` overrides `RUST_LOG`; both
//! accept the standard `env_logger` filter syntax (e.g. `picorecdsp=debug`).

use std::env;

/// Log levels recognised by [`init_logging`] and [`configured_level`].
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

impl From<LogLevel> for log::LevelFilter {
    fn from(l: LogLevel) -> Self {
        match l {
            LogLevel::Error => log::LevelFilter::Error,
            LogLevel::Warn => log::LevelFilter::Warn,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Debug => log::LevelFilter::Debug,
            LogLevel::Trace => log::LevelFilter::Trace,
        }
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
/// Installs `env_logger` as the global `log` backend.  The level is sourced
/// from `PICORECDSP_LOG`, falling back to `RUST_LOG`, then defaulting to
/// `info`.  Log lines are written to `stderr` with a timestamp, level, and
/// module path — suitable for capture by pCP's syslog.
///
/// # Idempotency
///
/// Safe to call more than once; subsequent calls are no-ops once the backend
/// has been installed.
pub fn init_logging() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let level = configured_level();
        // `PICORECDSP_LOG` takes precedence; if set, pass it as RUST_LOG so
        // env_logger picks it up (env_logger reads RUST_LOG natively).
        if let Ok(val) = env::var("PICORECDSP_LOG") {
            // SAFETY: single-threaded at startup; set before any other thread reads it.
            unsafe { env::set_var("RUST_LOG", &val) };
        }
        env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or(level_to_str(level)),
        )
        .format_timestamp_secs()
        .format_module_path(true)
        .target(env_logger::Target::Stderr)
        .init();
        log::info!("piCoreCDSP: logging initialised (level: {level:?})");
    });
}

fn level_to_str(l: LogLevel) -> &'static str {
    match l {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    }
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
