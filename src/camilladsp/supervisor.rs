//! Per-stream CamillaDSP supervisor for the ioplug backend.
//!
//! Implements the per-stream process lifecycle described in Gate 9 / M9:
//!
//! ```text
//! START(params)
//!   → adapt config
//!   → create pipe
//!   → spawn CamillaDSP with pipe read-end as stdin
//!   → READY (pipe write-end delivered to plugin via SCM_RIGHTS)
//!   → PCM flows: plugin → pipe → CamillaDSP → DAC
//!   → STOP (or plugin disconnect)
//!   → close our write-end → CamillaDSP sees EOF → exits
//! ```
//!
//! The supervisor holds the running `StdinPipeProcess` and exposes a simple
//! start / stop / health-check interface used by the ioplug controller loop.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::camilladsp::stdin_capture::StdinPipeProcess;
use crate::core::errors::AppResult;
use crate::core::logging::{log, LogLevel};

/// How long to wait for CamillaDSP to exit after the stream ends.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-stream CamillaDSP supervisor for the ioplug backend.
///
/// Create one instance per controller run.  Call `start_stream` when the
/// plugin sends START, and `stop_stream` (or let it drop) when the stream ends.
pub struct StdinSupervisor {
    /// Path to the `camilladsp` binary.
    binary_path: PathBuf,
    /// Path to the transient runtime-adapted YAML config file.
    runtime_config_path: PathBuf,
    /// Extra command-line arguments forwarded to CamillaDSP on every spawn
    /// (e.g. `--port 1234 --address 127.0.0.1 --statefile /…/statefile.yml`).
    cdsp_extra_args: Vec<String>,
    /// The currently active CamillaDSP process, if any.
    active: Option<StdinPipeProcess>,
    log_level: LogLevel,
}

impl StdinSupervisor {
    /// Create a new supervisor.
    ///
    /// `binary_path`: path to the `camilladsp` executable.
    /// `runtime_config_path`: path to the transient runtime-adapted YAML config
    ///   (will be canonicalised before being passed to CamillaDSP).
    pub fn new(
        binary_path: impl AsRef<Path>,
        runtime_config_path: impl AsRef<Path>,
        log_level: LogLevel,
    ) -> Self {
        Self {
            binary_path: binary_path.as_ref().to_path_buf(),
            runtime_config_path: runtime_config_path.as_ref().to_path_buf(),
            cdsp_extra_args: Vec::new(),
            active: None,
            log_level,
        }
    }

    /// Attach extra CamillaDSP command-line arguments to every spawn.
    ///
    /// Call this once after `new` before the first `start_stream`.  The
    /// arguments are forwarded verbatim after the config-path positional
    /// argument, e.g.:
    ///
    /// ```text
    /// camilladsp <config> --port 1234 --address 127.0.0.1 --statefile /…
    /// ```
    pub fn with_cdsp_args(mut self, args: Vec<String>) -> Self {
        self.cdsp_extra_args = args;
        self
    }

    /// Spawn a new CamillaDSP process for the current stream.
    ///
    /// The transient runtime config must already have been written/updated by
    /// the caller before invoking this method.
    ///
    /// Returns the raw fd of the pipe write-end, which the caller must pass
    /// to the plugin via SCM_RIGHTS in the READY message.  The fd remains
    /// valid until `stop_stream` is called or the supervisor is dropped.
    pub fn start_stream(&mut self) -> AppResult<std::os::unix::io::RawFd> {
        // Stop any leftover process from a previous (unexpected) stream.
        self.stop_stream_inner();

        let config_path =
            crate::camilladsp::stdin_capture::resolve_config_path(&self.runtime_config_path)?;

        log(
            LogLevel::Info,
            self.log_level,
            format!(
                "supervisor: spawning CamillaDSP '{}' with config '{}'",
                self.binary_path.display(),
                config_path.display()
            ),
        );

        let proc = StdinPipeProcess::spawn(&self.binary_path, &config_path, &self.cdsp_extra_args)?;
        let write_fd = proc.write_fd_raw();

        log(
            LogLevel::Debug,
            self.log_level,
            format!("supervisor: CamillaDSP spawned; pipe write-fd={write_fd}"),
        );

        self.active = Some(proc);
        Ok(write_fd)
    }

    /// Stop the active CamillaDSP process gracefully.
    ///
    /// Closes our copy of the pipe write-end (sending EOF to CamillaDSP once
    /// the plugin also closes its copy) and waits for the process to exit.
    /// No-op if no stream is active.
    pub fn stop_stream(&mut self) {
        self.stop_stream_inner();
    }

    /// Release the controller's duplicate of the active stdin pipe write-end.
    /// Call this immediately after READY+SCM_RIGHTS has been sent successfully.
    pub fn release_controller_write_end(&mut self) {
        if let Some(proc) = &mut self.active {
            proc.release_write_end();
            log(
                LogLevel::Debug,
                self.log_level,
                "supervisor: released controller copy of stdin pipe write-end",
            );
        }
    }

    /// Check whether the CamillaDSP process is still alive.
    ///
    /// Returns `true` if a process is running, `false` if it has exited or no
    /// process was started.
    pub fn is_running(&mut self) -> bool {
        match &mut self.active {
            Some(proc) => proc.is_running(),
            None => false,
        }
    }

    /// Poll for `timeout` to confirm that CamillaDSP is still running.
    ///
    /// Called immediately after `start_stream` to detect immediate crashes
    /// (bad config, device unavailable, etc.).  Polls `is_running` every
    /// `poll_interval` until either `timeout` elapses or the process exits.
    ///
    /// Returns `true` if the process is still alive at the end of the window,
    /// `false` if it exited.
    pub fn startup_check(&mut self, timeout: Duration, poll_interval: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_running() {
                return false;
            }
            if Instant::now() >= deadline {
                return true;
            }
            std::thread::sleep(poll_interval);
        }
    }

    // ── private ──────────────────────────────────────────────────────────

    fn stop_stream_inner(&mut self) {
        if let Some(proc) = self.active.take() {
            log(
                LogLevel::Info,
                self.log_level,
                "supervisor: stopping CamillaDSP (closing pipe write-end)",
            );
            if let Err(err) = proc.shutdown(SHUTDOWN_TIMEOUT) {
                log(
                    LogLevel::Warning,
                    self.log_level,
                    format!("supervisor: shutdown error: {err}"),
                );
            } else {
                log(
                    LogLevel::Debug,
                    self.log_level,
                    "supervisor: CamillaDSP exited",
                );
            }
        }
    }
}

impl Drop for StdinSupervisor {
    fn drop(&mut self) {
        self.stop_stream_inner();
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::RawFd;

    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        predicate()
    }

    fn dup_fd(fd: RawFd) -> RawFd {
        let duped = unsafe { libc::dup(fd) };
        assert!(duped >= 0);
        duped
    }

    fn tmp_config(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("picoredsp-supervisor-{name}.txt"));
        std::fs::write(&p, "dummy").unwrap();
        p
    }

    #[test]
    fn supervisor_starts_cat_and_stops_cleanly() {
        let config = tmp_config("start-stop");
        let mut sup = StdinSupervisor::new("/bin/cat", &config, LogLevel::Error);

        let write_fd = sup.start_stream().unwrap();
        assert!(write_fd >= 0);
        assert!(sup.is_running());

        sup.stop_stream();
        // After closing write-end, `cat` exits.
        assert!(!sup.is_running());
    }

    #[test]
    fn supervisor_start_stream_returns_valid_fd() {
        let config = tmp_config("fd-valid");
        let mut sup = StdinSupervisor::new("/bin/cat", &config, LogLevel::Error);
        let fd = sup.start_stream().unwrap();
        assert!(fd >= 0);
        // Write some bytes to confirm fd is usable.
        let n = unsafe { libc::write(fd, b"ping\n".as_ptr() as *const libc::c_void, 5) };
        assert_eq!(n, 5);
        sup.stop_stream();
    }

    #[test]
    fn supervisor_release_controller_write_end_allows_plugin_close_to_end_process() {
        let config = tmp_config("release-write-end");
        let mut sup = StdinSupervisor::new("/bin/cat", &config, LogLevel::Error);
        let fd = sup.start_stream().unwrap();
        let plugin_fd = dup_fd(fd);

        sup.release_controller_write_end();
        assert!(sup.is_running());

        let n = unsafe { libc::write(plugin_fd, b"x".as_ptr() as *const libc::c_void, 1) };
        assert_eq!(n, 1);
        unsafe {
            libc::close(plugin_fd);
        }

        assert!(
            wait_until(Duration::from_secs(1), || !sup.is_running()),
            "process should exit after the transferred write-end is closed"
        );

        sup.stop_stream();
    }

    #[test]
    fn supervisor_is_running_returns_false_when_no_stream() {
        let config = tmp_config("no-stream");
        let mut sup = StdinSupervisor::new("/bin/cat", &config, LogLevel::Error);
        assert!(!sup.is_running());
    }

    #[test]
    fn supervisor_stop_is_idempotent() {
        let config = tmp_config("idempotent");
        let mut sup = StdinSupervisor::new("/bin/cat", &config, LogLevel::Error);
        sup.start_stream().unwrap();
        sup.stop_stream();
        // Second stop must not panic.
        sup.stop_stream();
    }

    #[test]
    fn supervisor_start_stops_previous_process_on_restart() {
        let config = tmp_config("restart");
        let mut sup = StdinSupervisor::new("/bin/cat", &config, LogLevel::Error);

        // First stream.
        sup.start_stream().unwrap();
        assert!(sup.is_running());

        // Start again without stopping — the previous process must be stopped first.
        let fd2 = sup.start_stream().unwrap();
        assert!(fd2 >= 0);
        assert!(sup.is_running());

        sup.stop_stream();
    }

    #[test]
    fn supervisor_fails_with_nonexistent_binary() {
        let config = tmp_config("bad-bin");
        let mut sup = StdinSupervisor::new("/nonexistent/camilladsp", &config, LogLevel::Error);
        let result = sup.start_stream();
        assert!(result.is_err());
    }

    #[test]
    fn supervisor_fails_with_nonexistent_config() {
        let missing = std::env::temp_dir().join("picoredsp-missing-cfg.yml");
        let _ = std::fs::remove_file(&missing);
        let mut sup = StdinSupervisor::new("/bin/cat", &missing, LogLevel::Error);
        let result = sup.start_stream();
        assert!(result.is_err());
    }

    #[test]
    fn startup_check_returns_true_for_running_process() {
        // Write a temporary shell script that ignores its arguments and blocks
        // reading from stdin.  It will exit cleanly when we close the pipe
        // write-end (EOF), so stop_stream() does not need to kill it.
        let script = std::env::temp_dir().join("picoredsp-sup-read.sh");
        std::fs::write(&script, "#!/bin/sh\nread x\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = tmp_config("startup-ok");
        let mut sup = StdinSupervisor::new(&script, &config, LogLevel::Error);
        sup.start_stream().unwrap();

        let alive = sup.startup_check(Duration::from_millis(200), Duration::from_millis(20));
        assert!(
            alive,
            "stdin-read script should still be running after 200 ms"
        );

        // Closing the write-end sends EOF → `read` returns → script exits.
        sup.stop_stream();
    }

    #[test]
    fn startup_check_returns_false_when_process_exits_immediately() {
        // Write a temporary script that exits immediately (status 0).
        let script = std::env::temp_dir().join("picoredsp-sup-exit.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = tmp_config("startup-exits");
        let mut sup = StdinSupervisor::new(&script, &config, LogLevel::Error);
        sup.start_stream().unwrap();

        let alive = sup.startup_check(Duration::from_millis(500), Duration::from_millis(20));
        assert!(!alive, "process should have exited within the window");
    }
}
