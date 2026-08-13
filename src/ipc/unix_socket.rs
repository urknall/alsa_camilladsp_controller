//! AF_UNIX socket listener for the piCoreDSP IPC channel.
//!
//! This implements the controller-side transport for Gate 6:
//! - local AF_UNIX stream socket endpoint
//! - HELLO version negotiation
//! - bounded frame decode with timeout + disconnect handling
//! - READY / ERROR replies

use std::fs;
use std::io::{self, Read, Write};
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
}
