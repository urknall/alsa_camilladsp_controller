//! piCoreCDSP v2 — CamillaDSP-native controller library.
//!
//! See `piCoreCDSP_v2_Roadmap.md` and `ROADMAP_CHECKLIST_v2.md` at the repository
//! root for the architecture and gated implementation plan this crate follows.
//!
//! This crate currently only carries the Gate 2 scaffolding (ownership model,
//! `SourceState`, ALSA ingress contract, CamillaDSP transport contract). The
//! reconciliation loop (`reconcile`, Gate 3) has not been implemented yet.

pub mod camilla;
pub mod ownership;
pub mod source;
