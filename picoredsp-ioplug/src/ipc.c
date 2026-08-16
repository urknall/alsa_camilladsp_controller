/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * ipc.c — plugin ↔ Rust controller IPC over AF_UNIX socket
 *
 * This file implements the client side of the protocol documented in
 * ipc.h.  In the Gate 4 / M4 prototype the IPC functions are present
 * but the pcm.c null-sink path does not call them — CamillaDSP
 * integration is added in Gate 6 onwards.
 */

#include "ipc.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

/* -----------------------------------------------------------------------
 * Internal helpers
 * ---------------------------------------------------------------------- */

/*
 * send_all — write exactly `len` bytes, retrying on EINTR.
 * Returns 0 on success, -errno on failure.
 */
static int send_all(int fd, const void *buf, size_t len)
{
    const uint8_t *ptr = buf;
    while (len > 0) {
        ssize_t n = send(fd, ptr, len, MSG_NOSIGNAL);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -errno;
        }
        ptr += (size_t)n;
        len -= (size_t)n;
    }
    return 0;
}

/*
 * recv_all — read exactly `len` bytes with a millisecond timeout.
 * Returns 0 on success, -errno on failure, -ETIMEDOUT on timeout.
 */
static int recv_all(int fd, void *buf, size_t len, int timeout_ms)
{
    uint8_t *ptr = buf;
    while (len > 0) {
        struct pollfd pfd = { .fd = fd, .events = POLLIN };
        int rc = poll(&pfd, 1, timeout_ms);
        if (rc < 0) {
            if (errno == EINTR)
                continue;
            return -errno;
        }
        if (rc == 0)
            return -ETIMEDOUT;

        ssize_t n = recv(fd, ptr, len, 0);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -errno;
        }
        if (n == 0)
            return -ECONNRESET;

        ptr += (size_t)n;
        len -= (size_t)n;
    }
    return 0;
}

/* -----------------------------------------------------------------------
 * Public API
 * ---------------------------------------------------------------------- */

int pcdsp_ipc_connect(pcdsp_ipc_conn_t *conn, const char *socket_path)
{
    if (!conn)
        return -EINVAL;

    conn->fd                 = -1;
    conn->negotiated_version = 0;

    if (!socket_path)
        socket_path = PCDSP_IPC_DEFAULT_SOCKET_PATH;

    /* Create socket in non-blocking mode to implement the connect timeout. */
    int sfd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (sfd < 0)
        return -errno;

    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    size_t socket_path_len = strlen(socket_path);
    if (socket_path_len >= sizeof(addr.sun_path)) {
        close(sfd);
        return -ENAMETOOLONG;
    }
    memcpy(addr.sun_path, socket_path, socket_path_len + 1); /* NOLINT(clang-analyzer-security.insecureAPI.DeprecatedOrUnsafeBufferHandling) */

    int rc = connect(sfd, (struct sockaddr *)&addr, sizeof(addr));
    if (rc < 0) {
        if (errno == EINPROGRESS) {
            /* Wait for the connect to complete or time out. */
            struct pollfd pfd = { .fd = sfd, .events = POLLOUT };
            int pr;
            do {
                pr = poll(&pfd, 1, PCDSP_IPC_CONNECT_TIMEOUT_MS);
            } while (pr < 0 && errno == EINTR);

            if (pr == 0) {
                close(sfd);
                return -ETIMEDOUT;
            }
            if (pr < 0) {
                int e = errno;
                close(sfd);
                return -e;
            }
            /* Check whether the connection completed successfully. */
            int so_err = 0;
            socklen_t so_len = sizeof(so_err);
            if (getsockopt(sfd, SOL_SOCKET, SO_ERROR, &so_err, &so_len) < 0 ||
                so_err != 0) {
                int e = so_err ? so_err : errno;
                close(sfd);
                return -e;
            }
        } else {
            int e = errno;
            close(sfd);
            return -e;
        }
    }

    /* Restore blocking mode for subsequent send/recv calls. */
    {
        int flags = fcntl(sfd, F_GETFL, 0);
        if (flags < 0 || fcntl(sfd, F_SETFL, flags & ~O_NONBLOCK) < 0) {
            int e = errno;
            close(sfd);
            return -e;
        }
    }

    /* Send HELLO */
    pcdsp_msg_hello_t hello_out = {
        .type    = PCDSP_MSG_HELLO,
        .version = PCDSP_IPC_PROTOCOL_VERSION,
    };
    rc = send_all(sfd, &hello_out, sizeof(hello_out));
    if (rc < 0) {
        close(sfd);
        return rc;
    }

    /* Receive HELLO reply */
    pcdsp_msg_hello_t hello_in;
    rc = recv_all(sfd, &hello_in, sizeof(hello_in), PCDSP_IPC_IO_TIMEOUT_MS);
    if (rc < 0) {
        close(sfd);
        return rc;
    }
    if (hello_in.type != PCDSP_MSG_HELLO) {
        close(sfd);
        return -EPROTO;
    }

    /* The server must reply with a version no newer than the one we offered.
     * Accepting a higher value and silently clamping it locally would leave
     * the two peers with different negotiated-version state. */
    if (hello_in.version > PCDSP_IPC_PROTOCOL_VERSION) {
        close(sfd);
        return -EPROTO;
    }
    uint8_t negotiated = hello_in.version;
    if (negotiated < PCDSP_IPC_PROTOCOL_VERSION_MIN) {
        close(sfd);
        return -EPROTO;
    }

    conn->fd                 = sfd;
    conn->negotiated_version = negotiated;
    return 0;
}

void pcdsp_ipc_close(pcdsp_ipc_conn_t *conn)
{
    if (!conn)
        return;
    if (conn->fd >= 0) {
        close(conn->fd);
        conn->fd = -1;
    }
    conn->negotiated_version = 0;
}

int pcdsp_ipc_send_start(pcdsp_ipc_conn_t *conn,
                         uint32_t          rate,
                         uint8_t           format,
                         uint8_t           channels)
{
    if (!conn || conn->fd < 0)
        return -ENOTCONN;

    /* Encode the wire message explicitly in little-endian byte order as
     * required by the protocol definition in ipc.h.  Using a packed struct
     * literal and send_all() would transmit native machine byte order, which
     * is wrong on big-endian hosts. */
    uint8_t msg[8];
    msg[0] = PCDSP_MSG_START;
    msg[1] = conn->negotiated_version;
    msg[2] = (uint8_t)( rate        & 0xffu);
    msg[3] = (uint8_t)((rate >>  8) & 0xffu);
    msg[4] = (uint8_t)((rate >> 16) & 0xffu);
    msg[5] = (uint8_t)((rate >> 24) & 0xffu);
    msg[6] = format;
    msg[7] = channels;
    return send_all(conn->fd, msg, sizeof(msg));
}

int pcdsp_ipc_send_stop(pcdsp_ipc_conn_t *conn)
{
    if (!conn || conn->fd < 0)
        return -ENOTCONN;

    pcdsp_msg_stop_t msg = {
        .type    = PCDSP_MSG_STOP,
        .version = conn->negotiated_version,
    };
    return send_all(conn->fd, &msg, sizeof(msg));
}

int pcdsp_ipc_recv_ready(pcdsp_ipc_conn_t   *conn,
                         int                *pipe_fd,
                         pcdsp_error_code_t *error_code)
{
    if (!conn || conn->fd < 0)
        return -ENOTCONN;

    /* Peek at the message type byte first. */
    uint8_t type_byte = 0;
    int rc = recv_all(conn->fd, &type_byte, 1, PCDSP_IPC_READY_TIMEOUT_MS);
    if (rc < 0)
        return rc;

    if (type_byte == PCDSP_MSG_ERROR) {
        /* Read and validate the remaining error fields. */
        uint8_t rest[2] = { 0, 0 }; /* version + code */
        rc = recv_all(conn->fd, rest, sizeof(rest), PCDSP_IPC_READY_TIMEOUT_MS);
        if (rc < 0)
            return rc;
        if (rest[0] != conn->negotiated_version)
            return -EPROTO;
        if (rest[1] < (uint8_t)PCDSP_ERR_CONFIG ||
            rest[1] > (uint8_t)PCDSP_ERR_INTERNAL)
            return -EPROTO;
        if (error_code)
            *error_code = (pcdsp_error_code_t)rest[1];
        return -EPROTO;
    }

    if (type_byte != PCDSP_MSG_READY)
        return -EPROTO;

    /* Read the version byte that follows the type byte. */
    uint8_t ver_byte = 0;
    rc = recv_all(conn->fd, &ver_byte, 1, PCDSP_IPC_READY_TIMEOUT_MS);
    if (rc < 0)
        return rc;

    /* Reject version mismatches to enforce the negotiated protocol. */
    if (ver_byte != conn->negotiated_version)
        return -EPROTO;

    /*
     * Gate 7: if the caller does not need a pipe fd (pipe_fd == NULL), the
     * Rust controller sends a plain 2-byte READY with no SCM_RIGHTS follow-up.
     * Skip the recvmsg step entirely.
     *
     * Gate 8 will pass pipe_fd != NULL to receive the write end of the stdin
     * pipe from the controller via SCM_RIGHTS.
     */
    if (!pipe_fd)
        return 0;

    /* Gate 8+: receive the pipe write-end fd attached as ancillary data. */
    char    dummy;
    int     rfd = -1;
    char    cmsgbuf[CMSG_SPACE(sizeof(int))];
    struct iovec  iov = { .iov_base = &dummy, .iov_len = 1 };
    struct msghdr mh  = {
        .msg_iov        = &iov,
        .msg_iovlen     = 1,
        .msg_control    = cmsgbuf,
        .msg_controllen = sizeof(cmsgbuf),
    };

    struct pollfd pfd = { .fd = conn->fd, .events = POLLIN };
    while (1) {
        int pr = poll(&pfd, 1, PCDSP_IPC_READY_TIMEOUT_MS);
        if (pr < 0) {
            if (errno == EINTR)
                continue;
            return -errno;
        }
        if (pr == 0)
            return -ETIMEDOUT;
        break;
    }

    ssize_t n = recvmsg(conn->fd, &mh, MSG_CMSG_CLOEXEC);
    if (n < 0)
        return -errno;
    if (n == 0)
        return -ECONNRESET;

    /* Reject a truncated ancillary-data buffer outright: MSG_CTRUNC means
     * the kernel could not fit (or had to discard) some control data, so
     * any fd we might otherwise extract could be spurious/incomplete. Do
     * not attempt to walk cmsgbuf in that case. */
    if (mh.msg_flags & MSG_CTRUNC)
        return -EPROTO;

    for (struct cmsghdr *cm = CMSG_FIRSTHDR(&mh); cm; cm = CMSG_NXTHDR(&mh, cm)) {
        if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_RIGHTS) {
            /* Guard against a cmsg claiming to be shorter than one fd's
             * worth of payload before reading CMSG_DATA(cm); a malformed
             * or truncated header must not be trusted to contain a valid
             * int-sized fd. */
            if (cm->cmsg_len < CMSG_LEN(sizeof(int)))
                return -EPROTO;
            memcpy(&rfd, CMSG_DATA(cm), sizeof(int)); /* NOLINT(clang-analyzer-security.insecureAPI.DeprecatedOrUnsafeBufferHandling) */
            break;
        }
    }
    if (rfd < 0)
        return -EPROTO;

    /* Belt-and-suspenders: ensure CLOEXEC is set even on systems where
     * MSG_CMSG_CLOEXEC is unavailable or the kernel doesn't honour it. */
#ifdef FD_CLOEXEC
    (void)fcntl(rfd, F_SETFD, FD_CLOEXEC);
#endif

    *pipe_fd = rfd;
    return 0;
}
