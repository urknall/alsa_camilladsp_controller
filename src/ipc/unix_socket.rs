//! AF_UNIX socket listener for the piCoreDSP IPC channel.
//!
//! This implements the controller-side transport for Gates 6 and 8:
//! - local AF_UNIX stream socket endpoint
//! - HELLO version negotiation
//! - bounded frame decode with timeout + disconnect handling
//! - READY / ERROR replies
//! - READY with pipe write-end delivered via SCM_RIGHTS (Gate 8)

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::io::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::errors::{app_error, AppResult};
use crate::ipc::protocol::{
    expected_frame_len, negotiate_version, ErrorCode, PluginMessage, ProtocolError,
    PROTOCOL_VERSION,
};

#[derive(Debug, Clone)]
pub struct IpcServerConfig {
    pub io_timeout: Duration,
    pub max_message_len: usize,
}

impl Default for IpcServerConfig {
    fn default() -> Self {
        Self {
            io_timeout: Duration::from_secs(1),
            max_message_len: crate::ipc::protocol::MAX_MESSAGE_LEN,
        }
    }
}

/// AF_UNIX listener lifecycle owner.
pub struct IpcServer {
    socket_path: PathBuf,
    listener: UnixListener,
    config: IpcServerConfig,
}

impl IpcServer {
    pub fn bind(socket_path: impl AsRef<Path>, config: IpcServerConfig) -> AppResult<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();
        remove_stale_socket_file(&socket_path)?;

        let listener = UnixListener::bind(&socket_path).map_err(|err| {
            app_error(format!(
                "unable to bind AF_UNIX socket {}: {err}",
                socket_path.display()
            ))
        })?;
        Ok(Self {
            socket_path,
            listener,
            config,
        })
    }

    pub fn accept(&self) -> AppResult<IpcConnection> {
        let (stream, _) = self
            .listener
            .accept()
            .map_err(|err| app_error(format!("IPC accept failed: {err}")))?;
        Ok(IpcConnection::new(stream, self.config.clone()))
    }

    /// Try to accept a connection without blocking.  Returns `Ok(Some(_))` when a
    /// client is waiting, `Ok(None)` when none is available.
    pub fn try_accept(&self) -> AppResult<Option<IpcConnection>> {
        self.listener
            .set_nonblocking(true)
            .map_err(|err| app_error(format!("IPC set_nonblocking failed: {err}")))?;
        let result = loop {
            match self.listener.accept() {
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                other => break other,
            }
        };
        // Restore blocking mode regardless of outcome.
        let _ = self.listener.set_nonblocking(false);
        match result {
            Ok((stream, _)) => Ok(Some(IpcConnection::new(stream, self.config.clone()))),
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(err) => Err(app_error(format!("IPC accept failed: {err}"))),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// Single accepted plugin connection.
pub struct IpcConnection {
    stream: UnixStream,
    config: IpcServerConfig,
    negotiated_version: Option<u8>,
}

impl IpcConnection {
    fn new(stream: UnixStream, config: IpcServerConfig) -> Self {
        Self {
            stream,
            config,
            negotiated_version: None,
        }
    }

    /// Perform initial HELLO negotiation.
    pub fn perform_hello_handshake(&mut self) -> Result<u8, ProtocolError> {
        let hello = self.recv_plugin_message()?;
        let plugin_version = match hello {
            PluginMessage::Hello { version } => version,
            other => {
                return Err(ProtocolError::UnexpectedMessageType {
                    expected: crate::ipc::protocol::MessageType::Hello,
                    actual: other.message_type(),
                });
            }
        };

        let negotiated = negotiate_version(plugin_version, PROTOCOL_VERSION)?;
        self.send_message(&PluginMessage::Hello {
            version: negotiated,
        })?;
        self.negotiated_version = Some(negotiated);
        Ok(negotiated)
    }

    pub fn negotiated_version(&self) -> Option<u8> {
        self.negotiated_version
    }

    pub fn recv_plugin_message(&mut self) -> Result<PluginMessage, ProtocolError> {
        self.stream
            .set_read_timeout(Some(self.config.io_timeout))
            .map_err(ProtocolError::from)?;

        let mut type_byte = [0u8; 1];
        read_exact_checked(&mut self.stream, &mut type_byte)?;
        let expected = expected_frame_len(type_byte[0])?;
        if expected > self.config.max_message_len {
            return Err(ProtocolError::FrameTooLong {
                max: self.config.max_message_len,
                actual: expected,
            });
        }
        let mut frame = vec![0u8; expected];
        frame[0] = type_byte[0];
        if expected > 1 {
            read_exact_checked(&mut self.stream, &mut frame[1..])?;
        }
        PluginMessage::decode(&frame)
    }

    pub fn send_ready(&mut self) -> Result<(), ProtocolError> {
        let version = self
            .negotiated_version
            .ok_or(ProtocolError::HandshakeNotComplete)?;
        self.send_message(&PluginMessage::Ready { version })
    }

    /// Send READY and deliver `pipe_write_fd` to the plugin via `SCM_RIGHTS`.
    ///
    /// The plugin receives the fd as the write end of the stdin pipe.  It will
    /// write raw PCM directly into this fd; the fd is passed as ancillary data
    /// attached to a 1-byte dummy payload on the same `sendmsg` call that
    /// follows the plain 2-byte READY frame.
    ///
    /// Wire sequence:
    /// 1. Send 2-byte READY frame (`[0x04, version]`) via `write_all`.
    /// 2. Send a 1-byte dummy payload with the fd as `SCM_RIGHTS` ancillary
    ///    data via `sendmsg`.
    ///
    /// The C plugin's `pcdsp_ipc_recv_ready(conn, &pipe_fd, &err)` handles
    /// both steps on the receive side.
    pub fn send_ready_with_pipe_fd(&mut self, pipe_write_fd: RawFd) -> Result<(), ProtocolError> {
        use std::os::unix::io::AsRawFd;

        let version = self
            .negotiated_version
            .ok_or(ProtocolError::HandshakeNotComplete)?;

        // Step 1: send the plain READY frame.
        self.send_message(&PluginMessage::Ready { version })?;

        // Step 2: send the pipe fd via SCM_RIGHTS as a follow-up sendmsg.
        self.stream
            .set_write_timeout(Some(self.config.io_timeout))
            .map_err(ProtocolError::from)?;

        send_fd_via_scm_rights(self.stream.as_raw_fd(), pipe_write_fd).map_err(ProtocolError::from)
    }

    pub fn send_error(&mut self, code: ErrorCode) -> Result<(), ProtocolError> {
        let version = self
            .negotiated_version
            .ok_or(ProtocolError::HandshakeNotComplete)?;
        self.send_message(&PluginMessage::Error { version, code })
    }

    pub fn send_message(&mut self, msg: &PluginMessage) -> Result<(), ProtocolError> {
        let encoded = msg.encode();
        if encoded.len() > self.config.max_message_len {
            return Err(ProtocolError::FrameTooLong {
                max: self.config.max_message_len,
                actual: encoded.len(),
            });
        }
        self.stream
            .set_write_timeout(Some(self.config.io_timeout))
            .map_err(ProtocolError::from)?;
        self.stream.write_all(&encoded).map_err(ProtocolError::from)
    }
}

/// Send `fd` to the peer over `socket_fd` using `SCM_RIGHTS` ancillary data.
///
/// A 1-byte dummy payload is required by the kernel — ancillary-only messages
/// are silently dropped on some kernels.  The C plugin's `recvmsg` path
/// expects exactly this layout.
///
/// # Safety
/// `socket_fd` must be an open, connected `AF_UNIX` socket.  `fd` must be an
/// open file descriptor owned by the calling process.
fn send_fd_via_scm_rights(socket_fd: RawFd, fd: RawFd) -> io::Result<()> {
    // Build the control-message buffer: CMSG_SPACE(sizeof(int)) bytes.
    let cmsg_space =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) } as usize;
    let mut cmsg_buf: Vec<u8> = vec![0u8; cmsg_space];

    let dummy: u8 = 0;
    let mut iov = libc::iovec {
        iov_base: &dummy as *const u8 as *mut libc::c_void,
        iov_len: 1,
    };

    let mh = libc::msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
        msg_controllen: cmsg_buf.len() as _,
        msg_flags: 0,
    };

    // SAFETY: cmsg_buf is properly sized and aligned for a cmsghdr.
    let cm = unsafe { libc::CMSG_FIRSTHDR(&mh) };
    if cm.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "CMSG_FIRSTHDR returned null",
        ));
    }
    unsafe {
        (*cm).cmsg_level = libc::SOL_SOCKET;
        (*cm).cmsg_type = libc::SCM_RIGHTS;
        (*cm).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as usize;
        let data_ptr = libc::CMSG_DATA(cm) as *mut libc::c_int;
        data_ptr.write_unaligned(fd);
    }

    loop {
        // SAFETY: mh, iov, and cmsg_buf are valid for the duration of sendmsg.
        let n = unsafe { libc::sendmsg(socket_fd, &mh, libc::MSG_NOSIGNAL) };
        if n >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

fn remove_stale_socket_file(path: &Path) -> AppResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(app_error(format!(
            "unable to remove stale socket {}: {err}",
            path.display()
        ))),
    }
}

fn read_exact_checked(stream: &mut UnixStream, buf: &mut [u8]) -> Result<(), ProtocolError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Err(ProtocolError::Disconnected),
            Ok(n) => {
                filled += n;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(ProtocolError::from(err)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_socket_path(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "picoredsp-ipc-{test_name}-{}-{nanos}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn handshake_and_start_message_are_received() {
        let path = test_socket_path("handshake-start");
        let server = IpcServer::bind(&path, IpcServerConfig::default()).unwrap();

        let client_path = path.clone();
        let handle = thread::spawn(move || {
            let mut client = UnixStream::connect(client_path).unwrap();
            client
                .write_all(&PluginMessage::Hello { version: 1 }.encode())
                .unwrap();
            let mut hello_reply = [0u8; 2];
            client.read_exact(&mut hello_reply).unwrap();
            assert_eq!(
                PluginMessage::decode(&hello_reply).unwrap(),
                PluginMessage::Hello { version: 1 }
            );
            client
                .write_all(
                    &PluginMessage::Start {
                        version: 1,
                        rate: 48_000,
                        format: 2,
                        channels: 2,
                    }
                    .encode(),
                )
                .unwrap();
        });

        let mut conn = server.accept().unwrap();
        assert_eq!(conn.perform_hello_handshake().unwrap(), 1);
        assert_eq!(conn.negotiated_version(), Some(1));
        assert_eq!(
            conn.recv_plugin_message().unwrap(),
            PluginMessage::Start {
                version: 1,
                rate: 48_000,
                format: 2,
                channels: 2
            }
        );
        handle.join().unwrap();
    }

    #[test]
    fn recv_timeout_is_reported() {
        let path = test_socket_path("timeout");
        let server = IpcServer::bind(
            &path,
            IpcServerConfig {
                io_timeout: Duration::from_millis(40),
                ..IpcServerConfig::default()
            },
        )
        .unwrap();

        let client_path = path.clone();
        // Use a channel so the client keeps the socket open until we have
        // confirmed the timeout result, avoiding a race between the client
        // exiting (which would give Disconnected) and the timeout firing.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            let mut client = UnixStream::connect(client_path).unwrap();
            client
                .write_all(&PluginMessage::Hello { version: 1 }.encode())
                .unwrap();
            let mut hello_reply = [0u8; 2];
            client.read_exact(&mut hello_reply).unwrap();
            // Keep the connection alive until the server test is finished.
            done_rx.recv().ok();
        });

        let mut conn = server.accept().unwrap();
        conn.perform_hello_handshake().unwrap();
        let err = conn.recv_plugin_message().unwrap_err();
        // Signal client to exit before asserting so the handle can be joined.
        done_tx.send(()).ok();
        handle.join().unwrap();
        assert!(matches!(err, ProtocolError::Timeout));
    }

    #[test]
    fn peer_disconnect_is_reported() {
        let path = test_socket_path("disconnect");
        let server = IpcServer::bind(&path, IpcServerConfig::default()).unwrap();

        let client_path = path.clone();
        let handle = thread::spawn(move || {
            let mut client = UnixStream::connect(client_path).unwrap();
            client
                .write_all(&PluginMessage::Hello { version: 1 }.encode())
                .unwrap();
            let mut hello_reply = [0u8; 2];
            client.read_exact(&mut hello_reply).unwrap();
            drop(client);
        });

        let mut conn = server.accept().unwrap();
        conn.perform_hello_handshake().unwrap();
        let err = conn.recv_plugin_message().unwrap_err();
        assert!(matches!(err, ProtocolError::Disconnected));
        handle.join().unwrap();
    }

    #[test]
    fn unknown_message_type_is_rejected() {
        let path = test_socket_path("unknown-type");
        let server = IpcServer::bind(&path, IpcServerConfig::default()).unwrap();

        let client_path = path.clone();
        let handle = thread::spawn(move || {
            let mut client = UnixStream::connect(client_path).unwrap();
            client
                .write_all(&PluginMessage::Hello { version: 1 }.encode())
                .unwrap();
            let mut hello_reply = [0u8; 2];
            client.read_exact(&mut hello_reply).unwrap();
            client.write_all(&[0x7f, 1]).unwrap();
        });

        let mut conn = server.accept().unwrap();
        conn.perform_hello_handshake().unwrap();
        let err = conn.recv_plugin_message().unwrap_err();
        assert!(matches!(err, ProtocolError::UnknownMessageType(0x7f)));
        handle.join().unwrap();
    }

    #[test]
    fn send_ready_with_pipe_fd_delivers_fd_via_scm_rights() {
        use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

        let path = test_socket_path("scm-rights");
        let server = IpcServer::bind(&path, IpcServerConfig::default()).unwrap();

        // Create a pipe; the write-end will be passed via SCM_RIGHTS.
        let mut pipe_fds = [0i32; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let pipe_read = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
        let pipe_write_fd = pipe_fds[1]; // passed via SCM_RIGHTS; kept as raw

        let client_path = path.clone();
        let handle = thread::spawn(move || {
            let mut client = UnixStream::connect(client_path).unwrap();

            // HELLO handshake.
            client
                .write_all(&PluginMessage::Hello { version: 1 }.encode())
                .unwrap();
            let mut hello_reply = [0u8; 2];
            client.read_exact(&mut hello_reply).unwrap();

            // Send START.
            client
                .write_all(
                    &PluginMessage::Start {
                        version: 1,
                        rate: 48_000,
                        format: 10,
                        channels: 2,
                    }
                    .encode(),
                )
                .unwrap();

            // Receive plain READY frame (2 bytes).
            let mut ready_frame = [0u8; 2];
            client.read_exact(&mut ready_frame).unwrap();
            assert_eq!(ready_frame[0], 0x04); // READY type tag

            // Receive the SCM_RIGHTS follow-up via recvmsg.
            let cmsg_space =
                unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) } as usize;
            let mut cmsg_buf = vec![0u8; cmsg_space];
            let mut dummy_byte = 0u8;
            let mut iov = libc::iovec {
                iov_base: &mut dummy_byte as *mut u8 as *mut libc::c_void,
                iov_len: 1,
            };
            let mut mh = libc::msghdr {
                msg_name: std::ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: &mut iov,
                msg_iovlen: 1,
                msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
                msg_controllen: cmsg_buf.len() as _,
                msg_flags: 0,
            };
            let n = unsafe { libc::recvmsg(client.as_raw_fd(), &mut mh, 0) };
            assert!(
                n >= 0,
                "recvmsg failed: {}",
                std::io::Error::last_os_error()
            );

            let mut received_fd: libc::c_int = -1;
            let cm = unsafe { libc::CMSG_FIRSTHDR(&mh) };
            if !cm.is_null() {
                unsafe {
                    if (*cm).cmsg_level == libc::SOL_SOCKET && (*cm).cmsg_type == libc::SCM_RIGHTS {
                        std::ptr::copy_nonoverlapping(
                            libc::CMSG_DATA(cm),
                            &mut received_fd as *mut libc::c_int as *mut u8,
                            std::mem::size_of::<libc::c_int>(),
                        );
                    }
                }
            }
            assert!(
                received_fd >= 0,
                "did not receive a valid fd via SCM_RIGHTS"
            );

            // Write through the received fd and verify it appears on our pipe_read.
            let payload = b"gate8";
            let written = unsafe {
                libc::write(
                    received_fd,
                    payload.as_ptr() as *const libc::c_void,
                    payload.len(),
                )
            };
            assert_eq!(written as usize, payload.len());
            unsafe { libc::close(received_fd) };

            received_fd
        });

        let mut conn = server.accept().unwrap();
        conn.perform_hello_handshake().unwrap();
        let _ = conn.recv_plugin_message().unwrap(); // START

        conn.send_ready_with_pipe_fd(pipe_write_fd).unwrap();
        // Close the server-side copy of pipe_write_fd.
        unsafe { libc::close(pipe_write_fd) };

        let received_fd = handle.join().unwrap();
        assert!(received_fd >= 0);

        // Verify data flows through the pipe.
        let mut buf = [0u8; 5];
        let n = unsafe {
            libc::read(
                pipe_read.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                5,
            )
        };
        assert_eq!(n, 5);
        assert_eq!(&buf, b"gate8");
    }
}
