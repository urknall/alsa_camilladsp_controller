//! piCoreCDSP v2 — CamillaDSP-native controller library.
//!
//! See `piCoreCDSP_v2_Roadmap.md` and `ROADMAP_CHECKLIST_v2.md` at the repository
//! root for the architecture and gated implementation plan this crate follows.

pub mod camilla;
pub mod config_view;
pub mod error;
pub mod logging;
pub mod ownership;
pub mod rate_sync;
pub mod reconcile;
pub mod retry;
pub mod source;
