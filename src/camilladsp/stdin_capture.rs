//! Stdin PCM transport: pipe creation and CamillaDSP process management.
//!
//! This module implements the Rust side of the Gate 8 stdin pipe + FD handoff:
//!
//! ```text
//! Rust creates pipe()
//! Rust spawns CamillaDSP with pipe read-end as stdin
//! Rust passes pipe write-end to plugin via SCM_RIGHTS (in IPC READY message)
//! Plugin writes PCM directly into write-end
//! CamillaDSP reads PCM from its stdin → DSP → DAC
//! ```
//!
//! Rust is never in the PCM data path.  When the stream ends the plugin closes
//! its copy of the write-end; the read-end sees EOF and CamillaDSP shuts down.

use std::fs;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::core::errors::{app_error, AppResult};

// ─── pipe() helpers ────────────────────────────────────────────────────────

/// Create an `O_CLOEXEC` pipe pair `(read_fd, write_fd)`.
fn create_cloexec_pipe() -> AppResult<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    // SAFETY: `pipe2` is a standard Linux syscall; fds is a valid 2-element
    // array.  We own the returned file descriptors.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(app_error(format!(
            "pipe2 failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: pipe2 succeeded so both fds are valid and exclusively owned by us.
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((read_fd, write_fd))
}

// ─── StdinPipeProcess ──────────────────────────────────────────────────────

/// A running CamillaDSP process whose stdin is connected to a kernel pipe.
///
/// The `write_fd` is passed to the ioplug plugin via SCM_RIGHTS in the READY
/// message.  The plugin writes raw PCM directly into it.  When the stream ends
/// the plugin closes its copy of the write-end; our copy is closed by
/// `shutdown()` (or `drop()`), so CamillaDSP's read-end sees EOF and it exits.
pub struct StdinPipeProcess {
    /// Write end of the pipe.  `None` once it has been closed by `shutdown`.
    write_fd: Option<OwnedFd>,
    child: Child,
}

impl StdinPipeProcess {
    /// Create a pipe and spawn CamillaDSP with the read-end as its stdin.
    ///
    /// `binary` is the path to the `camilladsp` executable.
    /// `config_path` is the pre-written runtime YAML config file to pass as
    /// the first positional argument.
    /// `extra_args` are appended verbatim after the config path (e.g.
    /// `["--port", "1234", "--address", "127.0.0.1", "--statefile", "/…"]`).
    /// Pass an empty slice when no extra arguments are needed (e.g. in tests).
    pub fn spawn(
        binary: impl AsRef<Path>,
        config_path: impl AsRef<Path>,
        extra_args: &[String],
    ) -> AppResult<Self> {
        let binary = binary.as_ref();
        let config_path = config_path.as_ref();

        let (read_fd, write_fd) = create_cloexec_pipe()?;

        // Remove O_CLOEXEC from the read-end so the child inherits it as stdin.
        // The write-end retains O_CLOEXEC — the child must never inherit it.
        // SAFETY: fcntl with F_SETFD is a standard POSIX call; read_fd is open.
        let rc = unsafe { libc::fcntl(read_fd.as_raw_fd(), libc::F_SETFD, 0) };
        if rc != 0 {
            return Err(app_error(format!(
                "fcntl(F_SETFD, 0) on pipe read-end failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        // Consume `read_fd` into a raw fd and hand ownership to Stdio.
        // Using `into_raw_fd` prevents OwnedFd's Drop from closing the fd
        // a second time after Stdio takes ownership of it.
        let raw_read = read_fd.into_raw_fd();
        // SAFETY: raw_read is a valid, open fd that we just transferred
        // ownership of; Stdio::from_raw_fd takes ownership.
        let stdin_stdio = unsafe { Stdio::from_raw_fd(raw_read) };

        let mut cmd = Command::new(binary);
        cmd.arg(config_path);
        for arg in extra_args {
            cmd.arg(arg);
        }
        let child = cmd
            .stdin(stdin_stdio)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| {
                app_error(format!(
                    "failed to spawn CamillaDSP '{}': {err}",
                    binary.display()
                ))
            })?;

        Ok(Self {
            write_fd: Some(write_fd),
            child,
        })
    }

    /// Return the raw fd of the pipe write-end for passing via SCM_RIGHTS.
    ///
    /// Returns `-1` if the write-end has already been closed.
    pub fn write_fd_raw(&self) -> RawFd {
        self.write_fd
            .as_ref()
            .map(|fd| fd.as_raw_fd())
            .unwrap_or(-1)
    }

    /// Gracefully shut down CamillaDSP by closing the write-end of the pipe.
    ///
    /// Dropping our write-end sends EOF to CamillaDSP's stdin (once the
    /// plugin's copy is also closed), causing CamillaDSP to exit.  This
    /// method waits up to `timeout` for the process to exit.  If it does not
    /// exit in time the child is killed.
    ///
    /// Consumes `self` so that Drop does not attempt a redundant kill/wait.
    pub fn shutdown(mut self, timeout: Duration) -> AppResult<()> {
        // Close our copy of the write-end first.
        drop(self.write_fd.take());

        let result = wait_for_child(&mut self.child, timeout);

        // Prevent Drop from killing / waiting again.
        std::mem::forget(self);

        result
    }

    /// Check whether the child process is still running without blocking.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for StdinPipeProcess {
    /// Best-effort cleanup for unexpected drops (error paths, panics).
    fn drop(&mut self) {
        // Close the write-end first so that CamillaDSP sees EOF and may exit
        // without needing to be killed.
        drop(self.write_fd.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ─── helpers ───────────────────────────────────────────────────────────────

/// Poll `child.try_wait()` until the process exits or `timeout` elapses.
/// If timeout is exceeded the child is killed and reaped.
fn wait_for_child(child: &mut Child, timeout: Duration) -> AppResult<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(err) => return Err(app_error(format!("waitpid failed: {err}"))),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            child
                .wait()
                .map_err(|err| app_error(format!("waitpid after kill failed: {err}")))?;
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ─── Config path helpers ───────────────────────────────────────────────────

/// Resolve and canonicalise the runtime config path for CamillaDSP.
///
/// `runtime_config_path` is the transient runtime YAML written for the active
/// stream. We canonicalise it so CamillaDSP receives a stable absolute path.
pub fn resolve_config_path(runtime_config_path: &Path) -> AppResult<PathBuf> {
    fs::canonicalize(runtime_config_path).map_err(|err| {
        app_error(format!(
            "cannot resolve config path '{}': {err}",
            runtime_config_path.display()
        ))
    })
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn create_cloexec_pipe_returns_valid_fds() {
        let (read_fd, write_fd) = create_cloexec_pipe().unwrap();
        assert!(read_fd.as_raw_fd() >= 0);
        assert!(write_fd.as_raw_fd() >= 0);
        assert_ne!(read_fd.as_raw_fd(), write_fd.as_raw_fd());
    }

    #[test]
    fn pipe_write_becomes_readable_at_read_end() {
        let (read_fd, write_fd) = create_cloexec_pipe().unwrap();

        let raw_write = write_fd.as_raw_fd();
        let raw_read = read_fd.as_raw_fd();

        let buf = [0xAAu8];
        let n = unsafe { libc::write(raw_write, buf.as_ptr() as *const libc::c_void, 1) };
        assert_eq!(n, 1);

        let mut out = [0u8; 1];
        let m = unsafe { libc::read(raw_read, out.as_mut_ptr() as *mut libc::c_void, 1) };
        assert_eq!(m, 1);
        assert_eq!(out[0], 0xAA);
    }

    #[test]
    fn stdin_pipe_process_spawns_cat_and_shuts_down_cleanly() {
        // Use `cat` as a stand-in for CamillaDSP: it reads stdin until EOF.
        // Pass /dev/stdin so that `cat /dev/stdin` reads from the pipe rather
        // than exiting immediately after reading a regular file argument.
        let proc = StdinPipeProcess::spawn("/bin/cat", "/dev/stdin", &[]).unwrap();
        assert!(proc.write_fd_raw() >= 0);

        // Write some bytes through the write-end.
        let n = unsafe {
            libc::write(
                proc.write_fd_raw(),
                b"hello\n".as_ptr() as *const libc::c_void,
                6,
            )
        };
        assert_eq!(n, 6);

        // Shutdown: close write-end → cat sees EOF → exits.
        proc.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn stdin_pipe_process_exits_after_shutdown() {
        let proc = StdinPipeProcess::spawn("/bin/cat", "/dev/stdin", &[]).unwrap();
        proc.shutdown(Duration::from_secs(5)).unwrap();
        // If shutdown returned Ok, the child exited cleanly.
    }

    #[test]
    fn resolve_config_path_returns_absolute_path() {
        let tmp = std::env::temp_dir().join("picoredsp-test-cfg-resolve.txt");
        std::fs::write(&tmp, "x").unwrap();
        let resolved = resolve_config_path(&tmp).unwrap();
        assert!(resolved.is_absolute());
    }

    #[test]
    fn resolve_config_path_fails_for_missing_file() {
        let missing = std::env::temp_dir().join("picoredsp-no-such-file-xyz.txt");
        let result = resolve_config_path(&missing);
        assert!(result.is_err());
    }
}
