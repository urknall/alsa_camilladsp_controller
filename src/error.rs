use std::error::Error;

/// Shorthand `Result` type used throughout the crate.
pub type AppResult<T> = Result<T, Box<dyn Error>>;

/// Construct a generic `Box<dyn Error>` from any string message.
pub fn app_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(std::io::Error::new(std::io::ErrorKind::Other, message.into()))
}
