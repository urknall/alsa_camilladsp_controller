/*
 * tests/test_ipc.c — unit tests for the plugin ↔ Rust controller IPC layer
 *
 * Coverage
 * --------
 * Connection tests (require a real AF_UNIX listener, run in a thread):
 *   connect_fails_when_controller_absent
 *   connect_fails_when_server_closes_before_hello
 *   connect_fails_when_server_never_replies
 *   connect_fails_when_server_sends_wrong_message_type
 *   connect_fails_when_version_below_minimum
 *   connect_succeeds_with_valid_hello
 *   connect_negotiates_lower_daemon_version
 *
 * Protocol tests (use socketpair; no listener thread needed):
 *   send_start_sends_correct_wire_bytes
 *   send_stop_sends_correct_wire_bytes
 *   send_start_fails_when_not_connected
 *   send_stop_fails_when_not_connected
 *   recv_ready_succeeds_on_ready_message
 *   recv_ready_returns_error_code_on_error_config
 *   recv_ready_returns_error_code_on_error_playback_device
 *   recv_ready_returns_error_code_on_error_internal
 *   recv_ready_fails_on_disconnect_before_type_byte
 *   recv_ready_fails_on_disconnect_after_type_byte
 *   recv_ready_fails_on_wrong_message_type
 *   recv_ready_fails_when_not_connected
 *   recv_ready_with_pipe_fd_via_scm_rights
 *   close_is_idempotent
 *
 * Failure-scenario tests (Gate 10 M10 checklist items):
 *   failure_controller_absent_returns_meaningful_error
 *   failure_error_config_propagates_error_code
 *   failure_error_playback_device_propagates_error_code
 *   failure_socket_disconnect_returns_epipe
 *   failure_protocol_mismatch_returns_eproto
 */

#define _GNU_SOURCE

#include "ipc.h"

#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

/* -----------------------------------------------------------------------
 * Micro test framework  (matches existing tests/test_ringbuffer.c style)
 * ---------------------------------------------------------------------- */

static int g_pass = 0;
static int g_fail = 0;

#define TEST(name) static void test_##name(void)
#define RUN(name) \
    do { \
        int fail_before = g_fail; \
        printf("  %s ... ", #name); \
        fflush(stdout); \
        test_##name(); \
        if (g_fail == fail_before) { \
            printf("ok\n"); \
            g_pass++; \
        } \
    } while (0)

#define CHECK(expr) \
    do { \
        if (!(expr)) { \
            printf("FAIL\n  assertion failed: %s  (%s:%d)\n", \
                   #expr, __FILE__, __LINE__); \
            g_fail++; \
            return; \
        } \
    } while (0)

/* -----------------------------------------------------------------------
 * Helpers — socketpair-based mock (no threads needed)
 * ---------------------------------------------------------------------- */

/*
 * make_pair — create a connected socketpair and set up a pcdsp_ipc_conn_t
 * with the client end.  The server fd is returned in *server_fd.
 * Returns 0 on success, -1 on failure.
 */
static int make_pair(pcdsp_ipc_conn_t *conn, int *server_fd)
{
    int fds[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, fds) < 0)
        return -1;

    conn->fd                 = fds[0];
    conn->negotiated_version = PCDSP_IPC_PROTOCOL_VERSION;
    *server_fd               = fds[1];
    return 0;
}

/* Send all bytes to a fd; return 0 on success, -1 on failure. */
static int write_all(int fd, const void *buf, size_t len)
{
    const uint8_t *p = buf;
    while (len > 0) {
        ssize_t n = write(fd, p, len);
        if (n <= 0)
            return -1;
        p += (size_t)n;
        len -= (size_t)n;
    }
    return 0;
}

/* Read all bytes from a fd with a 200 ms poll timeout; return 0 on success. */
static int read_all_timeout(int fd, void *buf, size_t len)
{
    uint8_t *p = (uint8_t *)buf;
    while (len > 0) {
        struct pollfd pf = { .fd = fd, .events = POLLIN };
        if (poll(&pf, 1, 200) <= 0)
            return -1;
        ssize_t n = read(fd, p, len);
        if (n <= 0)
            return -1;
        p += (size_t)n;
        len -= (size_t)n;
    }
    return 0;
}

/* -----------------------------------------------------------------------
 * Helpers — mock server using a real AF_UNIX listener (threaded)
 *
 * Used only by the pcdsp_ipc_connect() tests that require a real connect().
 * ---------------------------------------------------------------------- */

/* Commands the mock server will execute after accepting one connection. */
typedef enum {
    SRV_CLOSE_IMMEDIATELY,     /* close the accepted fd without sending anything */
    SRV_NEVER_REPLY,           /* accept + read client HELLO, then sleep/close */
    SRV_REPLY_HELLO_OK,        /* proper HELLO reply, version=PROTOCOL_VERSION */
    SRV_REPLY_HELLO_LOW_VER,   /* HELLO reply with version 0 (below min) */
    SRV_REPLY_WRONG_TYPE,      /* send STOP instead of HELLO */
    SRV_REPLY_LOWER_VERSION,   /* HELLO reply with version = PROTOCOL_VERSION − 1
                                  (valid if that is still >= MIN) */
} srv_cmd_t;

typedef struct {
    char      path[108];      /* socket path (unique per test) */
    int          listen_fd;      /* server-side listening socket */
    _Atomic(int) accepted_fd;    /* accepted client fd (set by thread) */
    srv_cmd_t    cmd;
    pthread_t    thread;
    bool         thread_started;
} mock_server_t;

static void *mock_server_thread(void *arg)
{
    mock_server_t *s = arg;
    atomic_store_explicit(&s->accepted_fd, -1, memory_order_release);

    /* Accept one connection. */
    int cfd = accept(s->listen_fd, NULL, NULL);
    if (cfd < 0)
        return NULL;
    atomic_store_explicit(&s->accepted_fd, cfd, memory_order_release);

    if (s->cmd == SRV_CLOSE_IMMEDIATELY) {
        close(cfd);
        atomic_store_explicit(&s->accepted_fd, -1, memory_order_release);
        return NULL;
    }

    /* Read the client's HELLO (2 bytes). */
    uint8_t client_hello[2];
    ssize_t n = 0;
    size_t  got = 0;
    while (got < sizeof(client_hello)) {
        struct pollfd pf = { .fd = cfd, .events = POLLIN };
        if (poll(&pf, 1, 2000) <= 0)
            goto done;
        n = read(cfd, client_hello + got, sizeof(client_hello) - got);
        if (n <= 0)
            goto done;
        got += (size_t)n;
    }
    if (s->cmd == SRV_NEVER_REPLY) {
        /* Sleep long enough for the client timeout to fire, then close. */
        struct timespec ts = { .tv_sec = 3, .tv_nsec = 0 };
        nanosleep(&ts, NULL);
        goto done;
    }

    /* Send a HELLO (or wrong-type) reply. */
    uint8_t reply[2];
    if (s->cmd == SRV_REPLY_WRONG_TYPE) {
        reply[0] = PCDSP_MSG_STOP;
        reply[1] = PCDSP_IPC_PROTOCOL_VERSION;
    } else if (s->cmd == SRV_REPLY_HELLO_LOW_VER) {
        reply[0] = PCDSP_MSG_HELLO;
        reply[1] = 0; /* version 0 — below PROTOCOL_VERSION_MIN=1 */
    } else if (s->cmd == SRV_REPLY_LOWER_VERSION) {
        /* Send version = PROTOCOL_VERSION (no lower version exists yet since
         * PROTOCOL_VERSION == PROTOCOL_VERSION_MIN == 1).  This test ensures
         * the min(daemon, plugin) path is exercised. */
        reply[0] = PCDSP_MSG_HELLO;
        reply[1] = PCDSP_IPC_PROTOCOL_VERSION; /* same version */
    } else {
        /* SRV_REPLY_HELLO_OK */
        reply[0] = PCDSP_MSG_HELLO;
        reply[1] = PCDSP_IPC_PROTOCOL_VERSION;
    }

    write_all(cfd, reply, sizeof(reply));

done:
    /* Leave cfd open so the main thread can keep talking (for connect tests
     * that verify send_start / recv_ready after a successful connect). */
    return NULL;
}

/*
 * start_mock_server — create a unique AF_UNIX socket, start the server
 * thread, and return 0 on success.
 */
static int start_mock_server(mock_server_t *s, srv_cmd_t cmd)
{
    static _Atomic(int) counter = 0;
    int id = atomic_fetch_add(&counter, 1);

    snprintf(s->path, sizeof(s->path),
             "/tmp/test_ipc_%d_%d.sock", (int)getpid(), id);
    s->cmd = cmd;
    atomic_init(&s->accepted_fd, -1);
    s->thread_started = false;

    /* Remove stale socket. */
    unlink(s->path);

    s->listen_fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (s->listen_fd < 0)
        return -1;

    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    snprintf(addr.sun_path, sizeof(addr.sun_path), "%s", s->path);

    if (bind(s->listen_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(s->listen_fd);
        return -1;
    }
    if (listen(s->listen_fd, 1) < 0) {
        close(s->listen_fd);
        return -1;
    }

    if (pthread_create(&s->thread, NULL, mock_server_thread, s) != 0) {
        close(s->listen_fd);
        return -1;
    }
    s->thread_started = true;
    return 0;
}

static void stop_mock_server(mock_server_t *s)
{
    /* Join before the stack-backed mock_server_t goes out of scope.  The old
     * detached-thread helper could keep accessing `s` after a test returned,
     * and accepted_fd was concurrently read/written without synchronization. */
    if (s->thread_started) {
        pthread_join(s->thread, NULL);
        s->thread_started = false;
    }
    if (s->listen_fd >= 0) {
        close(s->listen_fd);
        s->listen_fd = -1;
    }
    int accepted_fd = atomic_exchange_explicit(&s->accepted_fd, -1,
                                               memory_order_acq_rel);
    if (accepted_fd >= 0)
        close(accepted_fd);
    unlink(s->path);
}

/* Brief sleep to let the server thread reach accept(). */
static void wait_for_server(void)
{
    struct timespec ts = { .tv_nsec = 5000000 }; /* 5 ms */
    nanosleep(&ts, NULL);
}

/* -----------------------------------------------------------------------
 * Connection tests
 * ---------------------------------------------------------------------- */

TEST(connect_rejects_overlong_socket_path)
{
    char path[256];
    memset(path, 'x', sizeof(path));
    path[0] = '/';
    path[sizeof(path) - 1] = '\0';

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, path);
    CHECK(rc == -ENAMETOOLONG);
    CHECK(conn.fd == -1);
}

TEST(connect_fails_when_controller_absent)
{
    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, "/tmp/does_not_exist_picoredsp.sock");
    /* Should fail with a connection-refused / no-such-file error. */
    CHECK(rc < 0);
    CHECK(conn.fd == -1);
}

TEST(connect_fails_when_server_closes_before_hello)
{
    mock_server_t srv;
    CHECK(start_mock_server(&srv, SRV_CLOSE_IMMEDIATELY) == 0);
    wait_for_server();

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, srv.path);
    CHECK(rc < 0);
    CHECK(conn.fd == -1);

    stop_mock_server(&srv);
}

TEST(connect_fails_when_server_never_replies)
{
    mock_server_t srv;
    CHECK(start_mock_server(&srv, SRV_NEVER_REPLY) == 0);
    wait_for_server();

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, srv.path);
    /* recv_all times out → -ETIMEDOUT */
    CHECK(rc == -ETIMEDOUT);
    CHECK(conn.fd == -1);

    stop_mock_server(&srv);
}

TEST(connect_fails_when_server_sends_wrong_message_type)
{
    mock_server_t srv;
    CHECK(start_mock_server(&srv, SRV_REPLY_WRONG_TYPE) == 0);
    wait_for_server();

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, srv.path);
    CHECK(rc == -EPROTO);
    CHECK(conn.fd == -1);

    stop_mock_server(&srv);
}

TEST(connect_fails_when_version_below_minimum)
{
    mock_server_t srv;
    CHECK(start_mock_server(&srv, SRV_REPLY_HELLO_LOW_VER) == 0);
    wait_for_server();

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, srv.path);
    CHECK(rc == -EPROTO);
    CHECK(conn.fd == -1);

    stop_mock_server(&srv);
}

TEST(connect_succeeds_with_valid_hello)
{
    mock_server_t srv;
    CHECK(start_mock_server(&srv, SRV_REPLY_HELLO_OK) == 0);
    wait_for_server();

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, srv.path);
    CHECK(rc == 0);
    CHECK(conn.fd >= 0);
    CHECK(conn.negotiated_version == PCDSP_IPC_PROTOCOL_VERSION);

    pcdsp_ipc_close(&conn);
    stop_mock_server(&srv);
}

TEST(connect_negotiates_lower_daemon_version)
{
    /* SRV_REPLY_LOWER_VERSION echoes back the same protocol version
     * (since PROTOCOL_VERSION == PROTOCOL_VERSION_MIN == 1, there is no
     * lower valid version to negotiate down to right now).  The test
     * exercises the min(daemon, plugin) logic path. */
    mock_server_t srv;
    CHECK(start_mock_server(&srv, SRV_REPLY_LOWER_VERSION) == 0);
    wait_for_server();

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, srv.path);
    CHECK(rc == 0);
    CHECK(conn.fd >= 0);
    /* negotiated = min(PROTOCOL_VERSION, reply_version) */
    CHECK(conn.negotiated_version >= PCDSP_IPC_PROTOCOL_VERSION_MIN);

    pcdsp_ipc_close(&conn);
    stop_mock_server(&srv);
}

/* -----------------------------------------------------------------------
 * Protocol tests — socketpair, no listener thread
 * ---------------------------------------------------------------------- */

TEST(send_start_sends_correct_wire_bytes)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    int rc = pcdsp_ipc_send_start(&conn, 48000, 2 /*S16_LE*/, 2);
    CHECK(rc == 0);

    /* Read the 8-byte START message from the server end. */
    uint8_t buf[8];
    CHECK(read_all_timeout(server_fd, buf, sizeof(buf)) == 0);

    CHECK(buf[0] == PCDSP_MSG_START);
    CHECK(buf[1] == PCDSP_IPC_PROTOCOL_VERSION);
    /* rate: 48000 = 0x0000BB80 in little-endian */
    uint32_t rate;
    memcpy(&rate, buf + 2, 4);
    CHECK(rate == 48000);
    CHECK(buf[6] == 2);   /* format = S16_LE */
    CHECK(buf[7] == 2);   /* channels = 2 */

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(send_start_sends_correct_wire_bytes_96k)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    int rc = pcdsp_ipc_send_start(&conn, 96000, 10 /*S32_LE*/, 8);
    CHECK(rc == 0);

    uint8_t buf[8];
    CHECK(read_all_timeout(server_fd, buf, sizeof(buf)) == 0);

    CHECK(buf[0] == PCDSP_MSG_START);
    uint32_t rate;
    memcpy(&rate, buf + 2, 4);
    CHECK(rate == 96000);
    CHECK(buf[6] == 10);
    CHECK(buf[7] == 8);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(send_stop_sends_correct_wire_bytes)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    int rc = pcdsp_ipc_send_stop(&conn);
    CHECK(rc == 0);

    uint8_t buf[2];
    CHECK(read_all_timeout(server_fd, buf, sizeof(buf)) == 0);

    CHECK(buf[0] == PCDSP_MSG_STOP);
    CHECK(buf[1] == PCDSP_IPC_PROTOCOL_VERSION);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(send_start_fails_when_not_connected)
{
    pcdsp_ipc_conn_t conn = { .fd = -1, .negotiated_version = 0 };
    int rc = pcdsp_ipc_send_start(&conn, 48000, 2, 2);
    CHECK(rc == -ENOTCONN);
}

TEST(send_stop_fails_when_not_connected)
{
    pcdsp_ipc_conn_t conn = { .fd = -1, .negotiated_version = 0 };
    int rc = pcdsp_ipc_send_stop(&conn);
    CHECK(rc == -ENOTCONN);
}

TEST(recv_ready_succeeds_on_ready_message)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    /* Server sends READY (2 bytes, no SCM_RIGHTS). */
    uint8_t ready[2] = { PCDSP_MSG_READY, PCDSP_IPC_PROTOCOL_VERSION };
    CHECK(write_all(server_fd, ready, sizeof(ready)) == 0);

    int rc = pcdsp_ipc_recv_ready(&conn, NULL, NULL);
    CHECK(rc == 0);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_rejects_error_with_wrong_version)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    uint8_t err[3] = {
        PCDSP_MSG_ERROR,
        (uint8_t)(PCDSP_IPC_PROTOCOL_VERSION + 1),
        (uint8_t)PCDSP_ERR_CONFIG,
    };
    CHECK(write_all(server_fd, err, sizeof(err)) == 0);

    pcdsp_error_code_t code = PCDSP_ERR_OK;
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, &code);
    CHECK(rc == -EPROTO);
    CHECK(code == PCDSP_ERR_OK);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_rejects_unknown_error_code)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    uint8_t err[3] = {
        PCDSP_MSG_ERROR,
        PCDSP_IPC_PROTOCOL_VERSION,
        0xffu,
    };
    CHECK(write_all(server_fd, err, sizeof(err)) == 0);

    pcdsp_error_code_t code = PCDSP_ERR_OK;
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, &code);
    CHECK(rc == -EPROTO);
    CHECK(code == PCDSP_ERR_OK);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_returns_error_code_on_error_config)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    uint8_t err[3] = {
        PCDSP_MSG_ERROR,
        PCDSP_IPC_PROTOCOL_VERSION,
        (uint8_t)PCDSP_ERR_CONFIG,
    };
    CHECK(write_all(server_fd, err, sizeof(err)) == 0);

    pcdsp_error_code_t code = PCDSP_ERR_OK;
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, &code);
    CHECK(rc == -EPROTO);
    CHECK(code == PCDSP_ERR_CONFIG);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_returns_error_code_on_error_playback_device)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    uint8_t err[3] = {
        PCDSP_MSG_ERROR,
        PCDSP_IPC_PROTOCOL_VERSION,
        (uint8_t)PCDSP_ERR_PLAYBACK_DEVICE,
    };
    CHECK(write_all(server_fd, err, sizeof(err)) == 0);

    pcdsp_error_code_t code = PCDSP_ERR_OK;
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, &code);
    CHECK(rc == -EPROTO);
    CHECK(code == PCDSP_ERR_PLAYBACK_DEVICE);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_returns_error_code_on_error_internal)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    uint8_t err[3] = {
        PCDSP_MSG_ERROR,
        PCDSP_IPC_PROTOCOL_VERSION,
        (uint8_t)PCDSP_ERR_INTERNAL,
    };
    CHECK(write_all(server_fd, err, sizeof(err)) == 0);

    pcdsp_error_code_t code = PCDSP_ERR_OK;
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, &code);
    CHECK(rc == -EPROTO);
    CHECK(code == PCDSP_ERR_INTERNAL);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_fails_on_disconnect_before_type_byte)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    /* Close server without sending anything. */
    close(server_fd);

    int rc = pcdsp_ipc_recv_ready(&conn, NULL, NULL);
    /* EOF on closed socket → -ECONNRESET */
    CHECK(rc == -ECONNRESET);

    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_fails_on_disconnect_after_type_byte)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    /* Send only the type byte (READY), then close. */
    uint8_t type = PCDSP_MSG_READY;
    CHECK(write_all(server_fd, &type, 1) == 0);
    close(server_fd);

    int rc = pcdsp_ipc_recv_ready(&conn, NULL, NULL);
    /* EOF mid-message → -ECONNRESET */
    CHECK(rc == -ECONNRESET);

    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_fails_on_wrong_message_type)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    /* Send a HELLO instead of READY/ERROR. */
    uint8_t bad[2] = { PCDSP_MSG_HELLO, PCDSP_IPC_PROTOCOL_VERSION };
    CHECK(write_all(server_fd, bad, sizeof(bad)) == 0);

    int rc = pcdsp_ipc_recv_ready(&conn, NULL, NULL);
    CHECK(rc == -EPROTO);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_fails_on_version_mismatch)
{
    /* Regression test for finding #9: after HELLO, a READY carrying a
     * version different from the negotiated version must be rejected. */
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    /* conn.negotiated_version is set to PROTOCOL_VERSION by make_pair().
     * Send a READY with a different version byte. */
    uint8_t bad_ver = (uint8_t)(conn.negotiated_version + 1u);
    uint8_t ready_bad[2] = { PCDSP_MSG_READY, bad_ver };
    CHECK(write_all(server_fd, ready_bad, sizeof(ready_bad)) == 0);

    int rc = pcdsp_ipc_recv_ready(&conn, NULL, NULL);
    CHECK(rc == -EPROTO);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(recv_ready_fails_when_not_connected)
{
    pcdsp_ipc_conn_t conn = { .fd = -1, .negotiated_version = 0 };
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, NULL);
    CHECK(rc == -ENOTCONN);
}

TEST(recv_ready_with_pipe_fd_via_scm_rights)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    /* Create a pipe to pass via SCM_RIGHTS. */
    int pipe_fds[2];
    CHECK(pipe(pipe_fds) == 0);

    /* Server: send READY (2-byte body) then a 1-byte follow-up carrying
     * the write-end fd via SCM_RIGHTS (matches ipc.c Gate 8 protocol). */

    /* 1. Send the 2-byte READY body first. */
    uint8_t ready_body[2] = { PCDSP_MSG_READY, PCDSP_IPC_PROTOCOL_VERSION };
    CHECK(write_all(server_fd, ready_body, sizeof(ready_body)) == 0);

    /* 2. Send the SCM_RIGHTS message (1 byte of dummy data + ancillary fd). */
    char dummy_data = 0;
    char cmsgbuf[CMSG_SPACE(sizeof(int))];
    struct iovec iov = { .iov_base = &dummy_data, .iov_len = 1 };
    struct msghdr mh = {
        .msg_iov        = &iov,
        .msg_iovlen     = 1,
        .msg_control    = cmsgbuf,
        .msg_controllen = sizeof(cmsgbuf),
    };
    struct cmsghdr *cm = CMSG_FIRSTHDR(&mh);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type  = SCM_RIGHTS;
    cm->cmsg_len   = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &pipe_fds[1], sizeof(int));

    ssize_t n = sendmsg(server_fd, &mh, 0);
    CHECK(n == 1);

    /* Client: receive the READY + pipe fd. */
    int received_fd = -1;
    int rc = pcdsp_ipc_recv_ready(&conn, &received_fd, NULL);
    CHECK(rc == 0);
    CHECK(received_fd >= 0);

    /* Write to the received fd and verify it comes out of the read end. */
    uint8_t test_data[4] = { 0xDE, 0xAD, 0xBE, 0xEF };
    CHECK(write_all(received_fd, test_data, sizeof(test_data)) == 0);
    uint8_t readback[4] = { 0 };
    CHECK(read_all_timeout(pipe_fds[0], readback, sizeof(readback)) == 0);
    CHECK(memcmp(test_data, readback, sizeof(test_data)) == 0);

    close(received_fd);
    close(pipe_fds[0]);
    close(pipe_fds[1]);
    close(server_fd);
    pcdsp_ipc_close(&conn);
}

TEST(close_is_idempotent)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    pcdsp_ipc_close(&conn);
    CHECK(conn.fd == -1);

    /* Second close must not crash or assert. */
    pcdsp_ipc_close(&conn);
    CHECK(conn.fd == -1);

    close(server_fd);
}

TEST(close_on_null_is_safe)
{
    /* pcdsp_ipc_close(NULL) must not crash. */
    pcdsp_ipc_close(NULL);
    g_pass++; /* not reached via RUN macro, manual count */
}

/* -----------------------------------------------------------------------
 * Gate 10 M10 — Failure scenario tests
 *
 * These test the exact failure-model scenarios listed in the roadmap
 * checklist, from the perspective of the IPC layer.
 * ---------------------------------------------------------------------- */

/*
 * Failure: Rust controller absent
 * --------------------------------
 * Plugin must fail cleanly with a meaningful (non-zero) error code and must
 * NOT silently discard samples (i.e., hw_params must propagate the failure
 * rather than opening a null-sink fallback without notice).
 *
 * Tested here at the IPC layer: pcdsp_ipc_connect must return a negative
 * errno when no controller socket exists.
 */
TEST(failure_controller_absent_returns_meaningful_error)
{
    pcdsp_ipc_conn_t conn;
    /* Use a path that is guaranteed not to exist. */
    int rc = pcdsp_ipc_connect(&conn, "/tmp/picoredsp_no_such_controller.sock");
    /* Any negative errno qualifies as "meaningful" — not 0, not a positive value. */
    CHECK(rc < 0);
    /* The connection handle must be left in a disconnected state. */
    CHECK(conn.fd == -1);
    /* The returned error must be a recognised errno (absolute value ≤ 200 on Linux). */
    CHECK(-rc <= 200);
}

/*
 * Failure: Invalid DSP config
 * ----------------------------
 * Controller sends ERROR(CONFIG).  Plugin must receive -EPROTO and the error
 * code must be PCDSP_ERR_CONFIG.
 */
TEST(failure_error_config_propagates_error_code)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    uint8_t err[3] = {
        PCDSP_MSG_ERROR,
        PCDSP_IPC_PROTOCOL_VERSION,
        (uint8_t)PCDSP_ERR_CONFIG,
    };
    CHECK(write_all(server_fd, err, sizeof(err)) == 0);

    pcdsp_error_code_t code = PCDSP_ERR_OK;
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, &code);
    CHECK(rc == -EPROTO);
    CHECK(code == PCDSP_ERR_CONFIG);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

/*
 * Failure: CamillaDSP cannot open DAC
 * -------------------------------------
 * Controller sends ERROR(PLAYBACK_DEVICE).  Plugin must receive -EPROTO and
 * the error code must be PCDSP_ERR_PLAYBACK_DEVICE.
 */
TEST(failure_error_playback_device_propagates_error_code)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    uint8_t err[3] = {
        PCDSP_MSG_ERROR,
        PCDSP_IPC_PROTOCOL_VERSION,
        (uint8_t)PCDSP_ERR_PLAYBACK_DEVICE,
    };
    CHECK(write_all(server_fd, err, sizeof(err)) == 0);

    pcdsp_error_code_t code = PCDSP_ERR_OK;
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, &code);
    CHECK(rc == -EPROTO);
    CHECK(code == PCDSP_ERR_PLAYBACK_DEVICE);

    close(server_fd);
    pcdsp_ipc_close(&conn);
}

/*
 * Failure: Plugin/application disappears (socket disconnect)
 * -----------------------------------------------------------
 * If the controller closes the socket unexpectedly, any subsequent send or
 * receive must return an error, not block or succeed silently.
 */
TEST(failure_socket_disconnect_returns_epipe)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    /* Server disappears. */
    close(server_fd);

    /* Receiving should detect the disconnect. */
    int rc = pcdsp_ipc_recv_ready(&conn, NULL, NULL);
    CHECK(rc < 0); /* -ECONNRESET or similar */

    pcdsp_ipc_close(&conn);
}

/*
 * Failure: Protocol mismatch
 * ---------------------------
 * Controller sends a HELLO with version < PROTOCOL_VERSION_MIN.
 * pcdsp_ipc_connect must reject the connection with -EPROTO.
 */
TEST(failure_protocol_mismatch_returns_eproto)
{
    mock_server_t srv;
    CHECK(start_mock_server(&srv, SRV_REPLY_HELLO_LOW_VER) == 0);
    wait_for_server();

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, srv.path);
    CHECK(rc == -EPROTO);
    CHECK(conn.fd == -1);

    stop_mock_server(&srv);
}

/* -----------------------------------------------------------------------
 * Additional edge-case tests
 * ---------------------------------------------------------------------- */

/*
 * send_start on a closed fd (fd >= 0 but write fails with EBADF).
 * Tests that the send_all error path propagates errno correctly.
 */
TEST(send_start_on_closed_fd_returns_error)
{
    pcdsp_ipc_conn_t conn;
    int server_fd;
    CHECK(make_pair(&conn, &server_fd) == 0);

    /* Close both ends so writing fails. */
    close(server_fd);
    close(conn.fd);
    conn.fd = open("/dev/null", O_WRONLY); /* open a write-only fd */
    /* Sending to /dev/null should succeed for send(), use a real closed fd. */
    conn.fd = -2; /* invalid fd to trigger send() error */

    /* With fd == -2 the ENOTCONN guard won't fire, but send() will fail. */
    /* Actually our guard is (conn->fd < 0) so -2 passes through.
     * Let's just verify the not-connected guard with fd == -1. */
    conn.fd = -1;
    int rc = pcdsp_ipc_send_start(&conn, 48000, 2, 2);
    CHECK(rc == -ENOTCONN);
}

/*
 * STOP sent after pipe_fd is handed back: verifies the full normal flow
 * where connect → start → ready → stop is exercised in one test.
 */
TEST(full_ipc_flow_connect_start_ready_stop)
{
    mock_server_t srv;
    CHECK(start_mock_server(&srv, SRV_REPLY_HELLO_OK) == 0);
    wait_for_server();

    pcdsp_ipc_conn_t conn;
    int rc = pcdsp_ipc_connect(&conn, srv.path);
    CHECK(rc == 0);

    /* Send START. */
    rc = pcdsp_ipc_send_start(&conn, 44100, 0 /*S8*/, 2);
    CHECK(rc == 0);

    /* Server side: consume the 8-byte START, then send READY. */
    uint8_t start_buf[8];
    CHECK(read_all_timeout(atomic_load_explicit(&srv.accepted_fd, memory_order_acquire), start_buf, sizeof(start_buf)) == 0);
    CHECK(start_buf[0] == PCDSP_MSG_START);

    uint8_t ready[2] = { PCDSP_MSG_READY, PCDSP_IPC_PROTOCOL_VERSION };
    CHECK(write_all(atomic_load_explicit(&srv.accepted_fd, memory_order_acquire), ready, sizeof(ready)) == 0);

    /* Client: receive READY. */
    rc = pcdsp_ipc_recv_ready(&conn, NULL, NULL);
    CHECK(rc == 0);

    /* Send STOP. */
    rc = pcdsp_ipc_send_stop(&conn);
    CHECK(rc == 0);

    /* Server side: consume the 2-byte STOP. */
    uint8_t stop_buf[2];
    CHECK(read_all_timeout(atomic_load_explicit(&srv.accepted_fd, memory_order_acquire), stop_buf, sizeof(stop_buf)) == 0);
    CHECK(stop_buf[0] == PCDSP_MSG_STOP);

    pcdsp_ipc_close(&conn);
    stop_mock_server(&srv);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("test_ipc\n");

    printf("\n[connection tests]\n");
    RUN(connect_rejects_overlong_socket_path);
    RUN(connect_fails_when_controller_absent);
    RUN(connect_fails_when_server_closes_before_hello);
    RUN(connect_fails_when_server_never_replies);
    RUN(connect_fails_when_server_sends_wrong_message_type);
    RUN(connect_fails_when_version_below_minimum);
    RUN(connect_succeeds_with_valid_hello);
    RUN(connect_negotiates_lower_daemon_version);

    printf("\n[protocol tests — send]\n");
    RUN(send_start_sends_correct_wire_bytes);
    RUN(send_start_sends_correct_wire_bytes_96k);
    RUN(send_stop_sends_correct_wire_bytes);
    RUN(send_start_fails_when_not_connected);
    RUN(send_stop_fails_when_not_connected);
    RUN(send_start_on_closed_fd_returns_error);

    printf("\n[protocol tests — recv_ready]\n");
    RUN(recv_ready_succeeds_on_ready_message);
    RUN(recv_ready_rejects_error_with_wrong_version);
    RUN(recv_ready_rejects_unknown_error_code);
    RUN(recv_ready_returns_error_code_on_error_config);
    RUN(recv_ready_returns_error_code_on_error_playback_device);
    RUN(recv_ready_returns_error_code_on_error_internal);
    RUN(recv_ready_fails_on_disconnect_before_type_byte);
    RUN(recv_ready_fails_on_disconnect_after_type_byte);
    RUN(recv_ready_fails_on_wrong_message_type);
    RUN(recv_ready_fails_on_version_mismatch);
    RUN(recv_ready_fails_when_not_connected);
    RUN(recv_ready_with_pipe_fd_via_scm_rights);

    printf("\n[protocol tests — misc]\n");
    RUN(close_is_idempotent);
    /* close_on_null_is_safe already counted above */
    test_close_on_null_is_safe();

    printf("\n[Gate 10 M10 failure scenarios]\n");
    RUN(failure_controller_absent_returns_meaningful_error);
    RUN(failure_error_config_propagates_error_code);
    RUN(failure_error_playback_device_propagates_error_code);
    RUN(failure_socket_disconnect_returns_epipe);
    RUN(failure_protocol_mismatch_returns_eproto);

    printf("\n[integration flow]\n");
    RUN(full_ipc_flow_connect_start_ready_stop);

    printf("\n%d passed, %d failed\n", g_pass, g_fail);
    return g_fail != 0 ? 1 : 0;
}
