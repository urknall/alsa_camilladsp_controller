use crate::backend::{AudioTransport, BackendProfile, StreamBackend, StreamDetector, StreamEvent};
use crate::core::errors::{app_error, AppResult};

/// Placeholder backend for future ioplug IPC stream events.
#[allow(dead_code)]
#[derive(Default)]
pub struct IoplugBackend;

impl IoplugBackend {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

impl StreamBackend for IoplugBackend {
    fn next_event(&mut self) -> AppResult<StreamEvent> {
        Err(app_error(
            "ioplug backend is not implemented yet (IPC event source placeholder)",
        ))
    }
}

impl BackendProfile for IoplugBackend {
    fn detector(&self) -> StreamDetector {
        StreamDetector::IoplugIpc
    }

    fn transport(&self) -> AudioTransport {
        // Placeholder target transport for the planned ioplug PCM path.
        AudioTransport::StdinPipe
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

    #[test]
    fn profile_reports_ioplug_detector_and_stdin_transport() {
        let backend = IoplugBackend::new();
        assert_eq!(backend.detector(), StreamDetector::IoplugIpc);
        assert_eq!(backend.transport(), AudioTransport::StdinPipe);
    }
}
