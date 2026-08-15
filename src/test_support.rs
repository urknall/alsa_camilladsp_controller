//! Shared helpers for opt-in, real-binary integration tests.
//!
//! This module only exists under `#[cfg(test)]` and is not part of the
//! shipped binary. It is intentionally small: a single helper shared by the
//! `#[ignore]`-gated live-CamillaDSP tests in `core::adaptation` and
//! `benchmark::collectors`, so both modules validate generated configs and
//! WebSocket queries against the exact same real CamillaDSP binary lookup
//! logic instead of maintaining two copies.

use std::env;
use std::path::PathBuf;

/// Locate a real CamillaDSP binary for opt-in live compatibility tests.
///
/// These tests are `#[ignore]`d by default — they need a real CamillaDSP
/// binary, which is not part of this repository or its normal `cargo test`
/// job — and must be run explicitly, e.g.
/// `PICOREDSP_TEST_CAMILLADSP_BIN=/path/to/camilladsp cargo test -- --ignored`.
/// If the environment variable is unset the test prints a skip notice and
/// returns rather than failing, so `--ignored` can still be run in
/// environments without the binary without spurious failures.
pub fn live_camilladsp_binary() -> Option<PathBuf> {
    let path = env::var_os("PICOREDSP_TEST_CAMILLADSP_BIN")?;
    let path = PathBuf::from(path);
    if !path.is_file() {
        eprintln!(
            "PICOREDSP_TEST_CAMILLADSP_BIN={} is not a file — skipping live CamillaDSP test",
            path.display()
        );
        return None;
    }
    Some(path)
}
