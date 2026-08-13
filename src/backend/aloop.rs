use crate::camilladsp::alsa_capture::DeviceListener;
use crate::backend::{detect_stream_event, StreamBackend, StreamEvent};
use crate::core::errors::AppResult;
use crate::core::config::{DeviceSnapshot, WaveFormat};
use std::thread;
use std::time::Duration;

/// Matches the Python listener's 50 ms debounce before reading ALSA controls.
const ALSA_DEBOUNCE_MS: u64 = 50;
/// Tiny guard delay used by `next_event()` to avoid a busy-spin if an
/// implementation returns immediate no-event polls repeatedly.
const NO_EVENT_SPIN_GUARD_MS: u64 = 1;

/// Stream backend adapter that turns HCTL snapshots into backend-neutral events.
pub struct AloopBackend<D: DeviceListener> {
    listener: D,
    current: DeviceSnapshot,
    fallback_wave: WaveFormat,
}

impl<D: DeviceListener> AloopBackend<D> {
    pub fn new(listener: D, initial: DeviceSnapshot, fallback_wave: WaveFormat) -> Self {
        Self {
            listener,
            current: initial,
            fallback_wave,
        }
    }

    /// Poll one controller tick and return a stream event when a transition happened.
    pub fn poll_event(&mut self, timeout_ms: u32) -> AppResult<Option<StreamEvent>> {
        if self.listener.wait_for_event(timeout_ms)? {
            thread::sleep(Duration::from_millis(ALSA_DEBOUNCE_MS));
            self.listener.handle_events()?;
        }

        let next = self.listener.read_snapshot()?;
        let event = detect_stream_event(&self.current, &next, &self.fallback_wave)?;
        self.current = next;
        Ok(event)
    }

    pub fn current_snapshot(&self) -> &DeviceSnapshot {
        &self.current
    }

    pub fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
        self.listener.read_snapshot()
    }
}

impl<D: DeviceListener> StreamBackend for AloopBackend<D> {
    fn next_event(&mut self) -> AppResult<StreamEvent> {
        const DEFAULT_POLL_TIMEOUT_MS: u32 = 200;
        loop {
            if let Some(event) = self.poll_event(DEFAULT_POLL_TIMEOUT_MS)? {
                return Ok(event);
            }
            thread::sleep(Duration::from_millis(NO_EVENT_SPIN_GUARD_MS));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::errors::{app_error, AppResult};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct MockListener {
        wait_results: RefCell<VecDeque<bool>>,
        snapshots: RefCell<VecDeque<DeviceSnapshot>>,
    }

    impl MockListener {
        fn new(wait_results: Vec<bool>, snapshots: Vec<DeviceSnapshot>) -> Self {
            Self {
                wait_results: RefCell::new(wait_results.into()),
                snapshots: RefCell::new(snapshots.into()),
            }
        }

        fn active(rate: u32, format: &str, channels: u32) -> DeviceSnapshot {
            DeviceSnapshot {
                active: true,
                wave: WaveFormat {
                    sample_rate: Some(rate),
                    sample_format: Some(format.to_owned()),
                    channels: Some(channels),
                },
            }
        }

        fn inactive() -> DeviceSnapshot {
            DeviceSnapshot {
                active: false,
                wave: WaveFormat::default(),
            }
        }
    }

    impl DeviceListener for MockListener {
        fn wait_for_event(&self, _timeout_ms: u32) -> AppResult<bool> {
            Ok(self.wait_results.borrow_mut().pop_front().unwrap_or(false))
        }

        fn handle_events(&self) -> AppResult<()> {
            Ok(())
        }

        fn read_snapshot(&self) -> AppResult<DeviceSnapshot> {
            self.snapshots
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| app_error("MockListener: no more snapshots"))
        }
    }

    #[test]
    fn poll_event_returns_started_event() {
        let initial = MockListener::inactive();
        let listener =
            MockListener::new(vec![false], vec![MockListener::active(48_000, "S32_LE", 2)]);
        let mut backend = AloopBackend::new(listener, initial, WaveFormat::default());

        let event = backend.poll_event(1).unwrap();
        assert_eq!(
            event,
            Some(StreamEvent::Started(crate::backend::StreamParams {
                rate: 48_000,
                format: "S32_LE".to_owned(),
                channels: 2,
            }))
        );
    }

    #[test]
    fn poll_event_returns_none_without_transition() {
        let initial = MockListener::active(44_100, "S16_LE", 2);
        let listener =
            MockListener::new(vec![false], vec![MockListener::active(44_100, "S16_LE", 2)]);
        let mut backend = AloopBackend::new(listener, initial, WaveFormat::default());

        let event = backend.poll_event(1).unwrap();
        assert_eq!(event, None);
    }

    #[test]
    fn next_event_skips_idle_ticks_until_transition() {
        let initial = MockListener::active(44_100, "S16_LE", 2);
        let listener = MockListener::new(
            vec![false, false],
            vec![
                MockListener::active(44_100, "S16_LE", 2),
                MockListener::active(96_000, "S24_4_LE", 2),
            ],
        );
        let mut backend = AloopBackend::new(listener, initial, WaveFormat::default());

        let event = backend.next_event().unwrap();
        assert_eq!(
            event,
            StreamEvent::Changed(crate::backend::StreamParams {
                rate: 96_000,
                format: "S24_4_LE".to_owned(),
                channels: 2,
            })
        );
    }
}
