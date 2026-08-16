//! Ownership model (roadmap `piCoreCDSP_v2_Roadmap.md` §4).
//!
//! There is no single owner of "the config" in piCoreCDSP. Every field or piece of
//! runtime state belongs to exactly one of four owners, and code elsewhere in this
//! crate must respect those boundaries: user/GUI-owned fields are never written by
//! Rust, ALSA-owned facts are never guessed at or overridden, CamillaDSP-owned
//! lifecycle state is never duplicated, and Rust's own temporary ownership is limited
//! to observation, reconciliation, and upstream workarounds.
//!
//! Central rule (roadmap §4):
//! > User config wins on configuration. ALSA wins on source rate. CamillaDSP owns the
//! > DSP lifecycle wherever upstream already can.

/// One of the four owners defined by the roadmap's ownership model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// §4.1 — the user, via CamillaGUI (or hand-edited config), owns persistent DSP
    /// configuration: filters, mixer, pipeline, FIR files, playback device, resampler,
    /// DSP/output sample rate, `chunksize`, `target_level`, volume, mute, and all other
    /// persistent DSP configuration.
    UserGui,
    /// §4.2 — ALSA owns audio transport facts: producer active/inactive state, the
    /// current nominal source sample rate, the actually negotiated format, and the
    /// actually negotiated channel count.
    Alsa,
    /// §4.3 — CamillaDSP owns capture, playback, DSP processing, buffering, clock
    /// drift, rate adjust, config validation, relative paths, `$samplerate$`/token
    /// resolution, device restarts, processing state, stop reason, statefile, config
    /// file path, and the runtime config lifecycle.
    CamillaDsp,
    /// §4.4 — Rust owns some things only temporarily: observation of `snd-aloop`,
    /// active/inactive detection, detection of the nominal source rate, reconciliation
    /// between ALSA and CamillaDSP, temporary source-rate synchronization, bounded
    /// retry/backoff, diagnostics, and workarounds for capabilities upstream does not
    /// yet provide. None of this is persisted as a competing source of truth.
    RustTemporary,
}

/// A single documented field/concern and the owner responsible for it, drawn directly
/// from roadmap §4.1-§4.4. This table exists so ownership is enforced by a reviewable,
/// testable artifact rather than convention alone (Gate 2 requirement).
pub struct OwnedField {
    pub name: &'static str,
    pub owner: Owner,
}

/// The full ownership table from roadmap §4. Keep this in sync with the roadmap text;
/// it is the canonical mapping other modules (and reviewers) should consult when
/// deciding who is allowed to read/write a given piece of state.
pub const FIELD_OWNERSHIP: &[OwnedField] = &[
    // §4.1 User / CamillaGUI own.
    OwnedField {
        name: "filters",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "mixer",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "pipeline",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "fir_files",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "playback_device",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "resampler",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "dsp_output_sample_rate",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "chunksize",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "target_level",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "volume",
        owner: Owner::UserGui,
    },
    OwnedField {
        name: "mute",
        owner: Owner::UserGui,
    },
    // §4.2 ALSA owns.
    OwnedField {
        name: "audio_transport",
        owner: Owner::Alsa,
    },
    OwnedField {
        name: "producer_active_state",
        owner: Owner::Alsa,
    },
    OwnedField {
        name: "nominal_source_sample_rate",
        owner: Owner::Alsa,
    },
    OwnedField {
        name: "negotiated_format",
        owner: Owner::Alsa,
    },
    OwnedField {
        name: "negotiated_channel_count",
        owner: Owner::Alsa,
    },
    // §4.3 CamillaDSP owns.
    OwnedField {
        name: "capture",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "playback",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "dsp_processing",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "buffering",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "clock_drift",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "rate_adjust",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "config_validation",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "relative_paths",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "samplerate_token_resolution",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "device_restarts",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "processing_state",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "stop_reason",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "statefile",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "config_file_path",
        owner: Owner::CamillaDsp,
    },
    OwnedField {
        name: "runtime_config_lifecycle",
        owner: Owner::CamillaDsp,
    },
    // §4.4 Rust owns only temporarily.
    OwnedField {
        name: "snd_aloop_observation",
        owner: Owner::RustTemporary,
    },
    OwnedField {
        name: "active_inactive_detection",
        owner: Owner::RustTemporary,
    },
    OwnedField {
        name: "nominal_source_rate_detection",
        owner: Owner::RustTemporary,
    },
    OwnedField {
        name: "alsa_camilladsp_reconciliation",
        owner: Owner::RustTemporary,
    },
    OwnedField {
        name: "temporary_source_rate_sync",
        owner: Owner::RustTemporary,
    },
    OwnedField {
        name: "bounded_retry_backoff",
        owner: Owner::RustTemporary,
    },
    OwnedField {
        name: "diagnostics",
        owner: Owner::RustTemporary,
    },
    OwnedField {
        name: "upstream_workarounds",
        owner: Owner::RustTemporary,
    },
];

/// Look up the documented owner of a named field/concern, if it is part of the
/// roadmap's ownership table.
pub fn owner_of(field: &str) -> Option<Owner> {
    FIELD_OWNERSHIP
        .iter()
        .find(|entry| entry.name == field)
        .map(|entry| entry.owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ownership_group_is_represented() {
        for owner in [
            Owner::UserGui,
            Owner::Alsa,
            Owner::CamillaDsp,
            Owner::RustTemporary,
        ] {
            assert!(
                FIELD_OWNERSHIP.iter().any(|entry| entry.owner == owner),
                "expected at least one field owned by {owner:?}"
            );
        }
    }

    #[test]
    fn field_names_are_unique() {
        let mut names: Vec<&str> = FIELD_OWNERSHIP.iter().map(|e| e.name).collect();
        let original_len = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            original_len,
            "duplicate field name in FIELD_OWNERSHIP"
        );
    }

    #[test]
    fn owner_of_known_and_unknown_fields() {
        assert_eq!(owner_of("volume"), Some(Owner::UserGui));
        assert_eq!(owner_of("nominal_source_sample_rate"), Some(Owner::Alsa));
        assert_eq!(owner_of("statefile"), Some(Owner::CamillaDsp));
        assert_eq!(owner_of("diagnostics"), Some(Owner::RustTemporary));
        assert_eq!(owner_of("not_a_real_field"), None);
    }
}
