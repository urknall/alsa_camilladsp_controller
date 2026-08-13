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

    int sfd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (sfd < 0)
        return -errno;

    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    strncpy(addr.sun_path, socket_path, sizeof(addr.sun_path) - 1);

    if (connect(sfd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        int e = errno;
        close(sfd);
        return -e;
    }

    /* Send HELLO */
    pcdsp_msg_hello_t hello_out = {
        .type    = PCDSP_MSG_HELLO,
        .version = PCDSP_IPC_PROTOCOL_VERSION,
    };
    int rc = send_all(sfd, &hello_out, sizeof(hello_out));
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

    uint8_t negotiated = hello_in.version < PCDSP_IPC_PROTOCOL_VERSION
                             ? hello_in.version
                             : PCDSP_IPC_PROTOCOL_VERSION;
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

    pcdsp_msg_start_t msg = {
        .type     = PCDSP_MSG_START,
        .version  = conn->negotiated_version,
        .rate     = rate,
        .format   = format,
        .channels = channels,
    };
    return send_all(conn->fd, &msg, sizeof(msg));
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
    uint8_t type_byte;
    int rc = recv_all(conn->fd, &type_byte, 1, PCDSP_IPC_IO_TIMEOUT_MS);
    if (rc < 0)
        return rc;

    if (type_byte == PCDSP_MSG_ERROR) {
        /* Read the remaining error fields. */
        uint8_t rest[2]; /* version + code */
        rc = recv_all(conn->fd, rest, sizeof(rest), PCDSP_IPC_IO_TIMEOUT_MS);
        if (rc < 0)
            return rc;
        if (error_code)
            *error_code = (pcdsp_error_code_t)rest[1];
        return -EPROTO;
    }

    if (type_byte != PCDSP_MSG_READY)
        return -EPROTO;

    /* READY: one version byte in the fixed part, then fd via SCM_RIGHTS.
     * We peek one byte for version already consumed — read remaining byte. */
    uint8_t ver_byte;
    rc = recv_all(conn->fd, &ver_byte, 1, PCDSP_IPC_IO_TIMEOUT_MS);
    if (rc < 0)
        return rc;

    /* Now receive the ancillary fd attached to an empty follow-up message. */
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
        int pr = poll(&pfd, 1, PCDSP_IPC_IO_TIMEOUT_MS);
        if (pr < 0) {
            if (errno == EINTR)
                continue;
            return -errno;
        }
        if (pr == 0)
            return -ETIMEDOUT;
        break;
    }

    ssize_t n = recvmsg(conn->fd, &mh, 0);
    if (n < 0)
        return -errno;

    for (struct cmsghdr *cm = CMSG_FIRSTHDR(&mh); cm; cm = CMSG_NXTHDR(&mh, cm)) {
        if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_RIGHTS) {
            memcpy(&rfd, CMSG_DATA(cm), sizeof(int));
            break;
        }
    }

    if (pipe_fd)
        *pipe_fd = rfd;
    return 0;
}
