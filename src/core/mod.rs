//! Core logic modules: state machine, configuration types, adaptation,
//! persistence, error handling, and logging.
//!
//! These modules are backend-neutral — they do not reference any ALSA or
//! CamillaDSP transport specifics.
pub mod adaptation;
pub mod config;
pub mod errors;
pub mod logging;
pub mod persistence;
pub mod state_machine;
