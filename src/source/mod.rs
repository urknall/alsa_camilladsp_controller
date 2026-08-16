//! Producer-independent audio ingress (roadmap `piCoreCDSP_v2_Roadmap.md` §5).
//!
//! piCoreCDSP knows no concrete producers (Squeezelite, AirPlay/Shairport Sync, or any
//! other ALSA application). All producers use the same ingress, `pcm.camilladsp`, and
//! the only thing Rust is allowed to observe about the source is captured by
//! [`SourceState`]. There is deliberately **no** `enum Producer { ... }` anywhere in
//! this crate: a new ALSA producer must never require a Rust code change.

pub mod alsa_loopback;
pub mod observer;

pub use observer::{SourceObserver, SourceSnapshot};

/// The only source abstraction piCoreCDSP knows (roadmap §5). Ownership of this state
/// belongs to ALSA (roadmap §4.2); Rust only observes and reports it, it never invents
/// or overrides it.
///
/// No producer-specific variants may be added here: piCoreCDSP does not know or care
/// which application (if any) is feeding `pcm.camilladsp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// No producer currently holds the source open.
    Inactive,
    /// A producer is actively streaming at the given nominal sample rate, as reported
    /// by `snd-aloop`.
    Active { sample_rate: u32 },
}

impl SourceState {
    /// Whether a producer is currently active.
    pub fn is_active(&self) -> bool {
        matches!(self, SourceState::Active { .. })
    }

    /// The nominal source sample rate, if a producer is active.
    pub fn sample_rate(&self) -> Option<u32> {
        match self {
            SourceState::Active { sample_rate } => Some(*sample_rate),
            SourceState::Inactive => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_reports_no_sample_rate() {
        let state = SourceState::Inactive;
        assert!(!state.is_active());
        assert_eq!(state.sample_rate(), None);
    }

    #[test]
    fn active_reports_its_sample_rate() {
        let state = SourceState::Active {
            sample_rate: 48_000,
        };
        assert!(state.is_active());
        assert_eq!(state.sample_rate(), Some(48_000));
    }
}
