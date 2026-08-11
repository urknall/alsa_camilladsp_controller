use crate::error::{app_error, AppResult};

/// Severity levels, ordered so that `Error < Warning < Info < Debug`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LogLevel {
    Error = 0,
    Warning = 1,
    Info = 2,
    Debug = 3,
}

impl LogLevel {
    /// Parse a log level from a string, accepting Python-style names for
    /// compatibility with the reference controller's `--loglevel` values.
    pub fn parse(value: &str) -> AppResult<Self> {
        match value.to_ascii_uppercase().as_str() {
            "CRITICAL" | "ERROR" => Ok(Self::Error),
            "WARNING" | "WARN" => Ok(Self::Warning),
            "INFO" => Ok(Self::Info),
            "DEBUG" => Ok(Self::Debug),
            other => Err(app_error(format!("invalid log level: {other}"))),
        }
    }
}

/// Emit a log message to stderr if `level <= configured`.
pub fn log(level: LogLevel, configured: LogLevel, message: impl AsRef<str>) {
    if level <= configured {
        let name = match level {
            LogLevel::Error => "ERROR",
            LogLevel::Warning => "WARNING",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
        };
        eprintln!("{name} - {}", message.as_ref());
    }
}
