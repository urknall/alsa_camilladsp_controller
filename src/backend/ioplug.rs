use crate::backend::{StreamBackend, StreamEvent};
use crate::core::errors::{app_error, AppResult};

/// Placeholder backend for future ioplug IPC stream events.
#[derive(Default)]
pub struct IoplugBackend;

impl IoplugBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StreamBackend for IoplugBackend {
    fn next_event(&mut self) -> AppResult<StreamEvent> {
        Err(app_error(
            "ioplug backend is not implemented yet (IPC event source placeholder)",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_backend_returns_not_implemented_error() {
        let mut backend = IoplugBackend::new();
        assert!(backend.next_event().is_err());
    }
}
