/*
 * tests/test_pcm_integration.c — ALSA ioplug plugin integration tests
 *
 * These tests exercise the picoredsp ALSA ioplug plugin end-to-end by
 * opening it via the ALSA API.  The plugin is loaded as a shared library
 * from the build directory (ALSA_PLUGIN_DIR must be set by the test runner,
 * see CMakeLists.txt).
 *
 * A mock IPC server runs in a background thread to handle the
 * HELLO / START / READY handshake for tests that need hw_params to
 * succeed.  Tests that verify failure paths use no server.
 *
 * Coverage
 * --------
 * open_close:
 *   plugin_opens_and_closes_without_controller
 *   rapid_open_close_does_not_leak_fds
 *
 * hw_params:
 *   hw_params_succeeds_with_mock_controller_and_ready
 *   hw_params_fails_with_error_config_from_controller
 *   hw_params_fails_with_error_playback_device_from_controller
 *   hw_params_fails_when_no_controller_present
 *
 * Unsupported format / channels:
 *   hw_params_fails_for_unsupported_format
 *
 * Poll descriptors / revents:
 *   poll_descriptors_count_is_one
 *   poll_revents_sets_pollout_when_space_available
 *
 * CamillaDSP early exit / DAC unavailable:
 *   hw_params_error_playback_device_propagates
 *   hw_params_error_config_propagates
 *
 * Drain timeout / pause synchronisation:
 *   drain_times_out_when_camilladsp_stops_reading_pipe
 *   pause_blocks_until_worker_stops_writing_before_returning
 *
 * sw_params avail_min / delay() pipe accounting:
 *   delay_accounts_for_frames_queued_in_kernel_pipe
 *   poll_revents_respects_avail_min_from_sw_params
 */

#define _GNU_SOURCE

#include "ipc.h"
#include "pcm_worker.h"

#include <alsa/asoundlib.h>
#include <alsa/pcm_external.h>

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
#include <time.h>
#include <unistd.h>

/* -----------------------------------------------------------------------
 * Micro test framework
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

/* Like CHECK, but for helpers that return a value instead of void (e.g.
 * measure_drain_timeout(), which is not itself a TEST() body). */
#define CHECK_RET(expr, retval) \
    do { \
        if (!(expr)) { \
            printf("FAIL\n  assertion failed: %s  (%s:%d)\n", \
                   #expr, __FILE__, __LINE__); \
            g_fail++; \
            return (retval); \
        } \
    } while (0)

/* -----------------------------------------------------------------------
 * ALSA config helpers
 * ---------------------------------------------------------------------- */

/* Unique socket path per test to prevent interference. */
static char g_sock_path[128];

static void init_sock_path(const char *tag)
{
    snprintf(g_sock_path, sizeof(g_sock_path),
             "/tmp/pcdsp-integ-%s-%d.sock", tag, (int)getpid());
}

/*
 * Open the picoredsp plugin via the ALSA config API.
 *
 * The plugin looks for a socket at `socket_path`.  We pass it via the ALSA
 * config string so the test can use a temporary per-test path.
 *
 * Returns 0 on success; snd_pcm_t** is set to the opened device.
 */
static int open_plugin(snd_pcm_t **pcm, const char *socket_path)
{
    /* Build an in-memory ALSA config. */
    char conf_str[256];
    snprintf(conf_str, sizeof(conf_str),
             "pcm.picoredsp_test { type picoredsp socket_path \"%s\" }",
             socket_path);

    snd_config_t *top = NULL;
    snd_config_update_ref(&top); /* load the global alsa.conf */

    snd_input_t *inp;
    if (snd_input_buffer_open(&inp, conf_str, (int)strlen(conf_str)) < 0)
        return -EINVAL;

    snd_config_t *extra = NULL;
    if (snd_config_top(&extra) < 0 ||
        snd_config_load(extra, inp) < 0) {
        snd_input_close(inp);
        snd_config_delete(top);
        return -EINVAL;
    }
    snd_input_close(inp);

    /* Merge extra into top */
    snd_config_merge(top, extra, 0);

    int rc = snd_pcm_open_lconf(pcm, "picoredsp_test",
                                 SND_PCM_STREAM_PLAYBACK, 0, top);
    snd_config_delete(top);
    return rc;
}

/*
 * Negotiate hw_params: stereo S16_LE at the given rate with a period near
 * `period_hint` frames and a buffer of 4 periods. Used to vary how much
 * audio can be queued so drain-timeout tests can prove the bound scales
 * with the backlog rather than being a single flat constant.
 */
static int set_hw_params_ex(snd_pcm_t *pcm, snd_pcm_uframes_t period_hint,
                             unsigned int rate)
{
    snd_pcm_hw_params_t *params;
    snd_pcm_hw_params_alloca(&params);
    snd_pcm_hw_params_any(pcm, params);

    int rc;
    if ((rc = snd_pcm_hw_params_set_access(pcm, params,
                 SND_PCM_ACCESS_RW_INTERLEAVED)) < 0) return rc;
    if ((rc = snd_pcm_hw_params_set_format(pcm, params,
                 SND_PCM_FORMAT_S16_LE)) < 0) return rc;
    if ((rc = snd_pcm_hw_params_set_channels(pcm, params, 2)) < 0)
        return rc;
    if ((rc = snd_pcm_hw_params_set_rate_near(pcm, params, &rate, 0)) < 0)
        return rc;
    snd_pcm_uframes_t period = period_hint;
    if ((rc = snd_pcm_hw_params_set_period_size_near(pcm, params, &period, 0)) < 0)
        return rc;
    snd_pcm_uframes_t buf = period * 4;
    if ((rc = snd_pcm_hw_params_set_buffer_size_near(pcm, params, &buf)) < 0)
        return rc;
    return snd_pcm_hw_params(pcm, params);
}

/*
 * Negotiate hw_params: stereo S16_LE at 48 kHz with a 1024-frame period.
 */
static int set_hw_params(snd_pcm_t *pcm)
{
    return set_hw_params_ex(pcm, 1024, 48000);
}

/* -----------------------------------------------------------------------
 * Mock IPC server helpers
 * ---------------------------------------------------------------------- */

typedef struct {
    const char         *sock_path;
    pcdsp_error_code_t  response;   /* PCDSP_ERR_OK → send READY; else send ERROR */
    bool                drain_pipe; /* read from transferred pipe until EOF */
    /* Incremented (if non-NULL) by the number of bytes read whenever
     * drain_pipe is true; lets a test observe whether data is still
     * flowing after some synchronisation point (e.g. pause()). */
    _Atomic(long)      *bytes_read;
    /* Keep the transferred pipe's read end open without ever reading from
     * it, simulating a CamillaDSP that is alive but wedged (does not
     * consume stdin). Combined with a non-blocking pipe write, this means
     * write() never gets POLLOUT again once the kernel pipe fills, and no
     * -EPIPE is ever produced — used to exercise pcdsp_drain()'s timeout. */
    bool                hold_pipe_no_read;
    /* Checked periodically while hold_pipe_no_read is active so the test
     * can let the server thread return promptly once it is done, instead
     * of waiting out the full hold duration. */
    _Atomic(bool)      *stop_holding;
    /* When > 0: drain the pipe like drain_pipe, but close the read end as
     * soon as this many bytes have been read, simulating CamillaDSP exiting
     * mid-stream (kernel closes the read end, so the worker's next write
     * gets -EPIPE) rather than a clean drain_pipe-until-EOF shutdown. */
    long                close_pipe_after_bytes;
} server_args_t;

/* Upper bound (in 10 ms steps) on how long hold_pipe_no_read keeps the pipe
 * read end open without reading; comfortably longer than
 * PCDSP_DRAIN_TIMEOUT_NS (5 s) so the drain() timeout is exercised first. */
#define HOLD_PIPE_MAX_STEPS 800 /* 800 * 10ms = 8s */

static int send_fd_with_ready(int socket_fd, int send_fd)
{
    char dummy = 0;
    char cmsgbuf[CMSG_SPACE(sizeof(int))];
    struct iovec iov = {
        .iov_base = &dummy,
        .iov_len  = 1,
    };
    struct msghdr msg = {
        .msg_iov        = &iov,
        .msg_iovlen     = 1,
        .msg_control    = cmsgbuf,
        .msg_controllen = sizeof(cmsgbuf),
    };

    memset(cmsgbuf, 0, sizeof(cmsgbuf));
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    if (!cm)
        return -1;
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &send_fd, sizeof(send_fd));

    return sendmsg(socket_fd, &msg, MSG_NOSIGNAL) == 1 ? 0 : -1;
}

/*
 * Simple one-shot IPC server: accept one connection, complete the HELLO
 * handshake, then send READY or an ERROR depending on `args->response`.
 */
static void *mock_server_thread(void *arg)
{
    server_args_t *a = arg;

    /* Remove any stale socket */
    unlink(a->sock_path);

    int sfd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (sfd < 0) return NULL;

    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    strncpy(addr.sun_path, a->sock_path, sizeof(addr.sun_path) - 1);
    if (bind(sfd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(sfd);
        return NULL;
    }
    listen(sfd, 1);

    int cfd = accept(sfd, NULL, NULL);
    close(sfd);
    if (cfd < 0) return NULL;

    /* HELLO exchange */
    pcdsp_msg_hello_t hello;
    if (recv(cfd, &hello, sizeof(hello), MSG_WAITALL) != sizeof(hello) ||
        hello.type != PCDSP_MSG_HELLO) {
        close(cfd);
        return NULL;
    }
    pcdsp_msg_hello_t reply_hello = {
        .type    = PCDSP_MSG_HELLO,
        .version = hello.version,
    };
    send(cfd, &reply_hello, sizeof(reply_hello), MSG_NOSIGNAL);

    /* START message */
    pcdsp_msg_start_t start;
    if (recv(cfd, &start, sizeof(start), MSG_WAITALL) != sizeof(start) ||
        start.type != PCDSP_MSG_START) {
        close(cfd);
        return NULL;
    }

    if (a->response == PCDSP_ERR_OK) {
        pcdsp_msg_ready_t ready = {
            .type    = PCDSP_MSG_READY,
            .version = hello.version,
        };
        send(cfd, &ready, sizeof(ready), MSG_NOSIGNAL);
        int pipefd[2];
        if (pipe(pipefd) == 0) {
            if (a->hold_pipe_no_read) {
                /* Shrink the kernel pipe to the minimum (typically one page)
                 * so that even the small buffer negotiated by set_hw_params
                 * overflows it quickly once nobody reads — otherwise the
                 * default 64 KiB pipe would comfortably absorb the whole
                 * ALSA-level buffer and writes would never actually block. */
                fcntl(pipefd[1], F_SETPIPE_SZ, 4096);
            }
            (void)send_fd_with_ready(cfd, pipefd[1]);
            close(pipefd[1]);
            if (a->close_pipe_after_bytes > 0) {
                uint8_t tmp[4096];
                ssize_t n;
                long total = 0;
                while (total < a->close_pipe_after_bytes &&
                       (n = read(pipefd[0], tmp, sizeof(tmp))) > 0) {
                    total += n;
                    if (a->bytes_read)
                        atomic_fetch_add_explicit(a->bytes_read, (long)n,
                                                   memory_order_relaxed);
                }
                /* Simulate CamillaDSP exiting mid-stream: close the read end
                 * now instead of draining to EOF, so the worker's next
                 * write() observes -EPIPE. */
            } else if (a->drain_pipe) {
                uint8_t tmp[4096];
                ssize_t n;
                while ((n = read(pipefd[0], tmp, sizeof(tmp))) > 0) {
                    if (a->bytes_read)
                        atomic_fetch_add_explicit(a->bytes_read, (long)n,
                                                   memory_order_relaxed);
                }
            } else if (a->hold_pipe_no_read) {
                for (int i = 0; i < HOLD_PIPE_MAX_STEPS; i++) {
                    if (a->stop_holding &&
                        atomic_load_explicit(a->stop_holding, memory_order_acquire))
                        break;
                    struct timespec step_ts = { .tv_nsec = 10000000L }; /* 10 ms */
                    nanosleep(&step_ts, NULL);
                }
            }
            close(pipefd[0]);
        }
    } else {
        pcdsp_msg_error_t err = {
            .type    = PCDSP_MSG_ERROR,
            .version = hello.version,
            .code    = (uint8_t)a->response,
        };
        send(cfd, &err, sizeof(err), MSG_NOSIGNAL);
    }

    /* Wait briefly for the plugin to process the response */
    struct timespec ts = { .tv_sec = 0, .tv_nsec = 100000000L }; /* 100 ms */
    nanosleep(&ts, NULL);
    close(cfd);
    return NULL;
}

/*
 * Start a mock server thread and give it 50 ms to bind its socket before
 * returning.  The caller must pthread_join the returned thread id.
 */
static pthread_t start_mock_server(server_args_t *args)
{
    pthread_t tid;
    pthread_create(&tid, NULL, mock_server_thread, args);
    /* Give the server time to bind. */
    struct timespec ts = { .tv_sec = 0, .tv_nsec = 50000000L }; /* 50 ms */
    nanosleep(&ts, NULL);
    return tid;
}

/* -----------------------------------------------------------------------
 * open / close tests
 * ---------------------------------------------------------------------- */

TEST(plugin_opens_and_closes_without_controller)
{
    /*
     * snd_pcm_open() must succeed (the plugin allocates internal state
     * and creates the eventfd).  The IPC connection is deferred until
     * hw_params, so no controller is needed for open/close alone.
     */
    init_sock_path("open-close");
    snd_pcm_t *pcm = NULL;
    int rc = open_plugin(&pcm, g_sock_path);
    CHECK(rc == 0);
    CHECK(pcm != NULL);

    snd_pcm_close(pcm);
}

TEST(rapid_open_close_does_not_leak_fds)
{
    /*
     * Open and immediately close the plugin 20 times in a loop.
     * If the eventfd or IPC socket is leaked each iteration, the test
     * process will exhaust its file descriptor limit.
     */
    init_sock_path("rapid-open");
    for (int i = 0; i < 20; i++) {
        snd_pcm_t *pcm = NULL;
        int rc = open_plugin(&pcm, g_sock_path);
        CHECK(rc == 0);
        snd_pcm_close(pcm);
    }
}

/* -----------------------------------------------------------------------
 * hw_params tests
 * ---------------------------------------------------------------------- */

TEST(hw_params_fails_when_no_controller_present)
{
    /*
     * With no controller running at the socket path, hw_params must fail
     * (the IPC connect will time out or be refused).  The return value must
     * be negative; the plugin must not block indefinitely.
     */
    init_sock_path("no-ctrl");
    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);

    int rc = set_hw_params(pcm);
    CHECK(rc < 0);

    snd_pcm_close(pcm);
}

TEST(hw_params_succeeds_with_mock_controller_and_ready)
{
    /*
     * With a mock controller that responds with READY plus the required
     * SCM_RIGHTS pipe fd, hw_params must
     * succeed (return 0).
     */
    init_sock_path("hw-ready");
    server_args_t args = { .sock_path = g_sock_path, .response = PCDSP_ERR_OK };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);

    int rc = set_hw_params(pcm);
    CHECK(rc == 0);

    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

TEST(hw_params_fails_with_error_config_from_controller)
{
    /*
     * With a mock controller that responds with ERROR_CONFIG, hw_params
     * must fail with -EINVAL.
     */
    init_sock_path("hw-cfg-err");
    server_args_t args = {
        .sock_path = g_sock_path,
        .response  = PCDSP_ERR_CONFIG,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);

    int rc = set_hw_params(pcm);
    CHECK(rc == -EINVAL);

    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

TEST(hw_params_fails_with_error_playback_device_from_controller)
{
    /*
     * With a mock controller that responds with ERROR_PLAYBACK_DEVICE,
     * hw_params must fail with -ENODEV.  This simulates
     * the "CamillaDSP cannot open DAC" failure scenario.
     */
    init_sock_path("hw-dac-err");
    server_args_t args = {
        .sock_path = g_sock_path,
        .response  = PCDSP_ERR_PLAYBACK_DEVICE,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);

    int rc = set_hw_params(pcm);
    CHECK(rc == -ENODEV);

    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

/* -----------------------------------------------------------------------
 * Unsupported format / channels
 * ---------------------------------------------------------------------- */

TEST(hw_params_fails_for_unsupported_format)
{
    /*
     * Attempt to negotiate MU_LAW format, which our plugin does not
     * support.  snd_pcm_hw_params_set_format() or snd_pcm_hw_params()
     * must return a negative error code without crashing.
     */
    init_sock_path("unsup-fmt");
    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);

    snd_pcm_hw_params_t *params;
    snd_pcm_hw_params_alloca(&params);
    snd_pcm_hw_params_any(pcm, params);

    int rc = snd_pcm_hw_params_set_format(pcm, params, SND_PCM_FORMAT_MU_LAW);
    CHECK(rc < 0); /* ALSA constraint check should reject it */

    snd_pcm_close(pcm);
}

/* -----------------------------------------------------------------------
 * Poll descriptors / revents
 * ---------------------------------------------------------------------- */

TEST(poll_descriptors_count_is_one)
{
    /*
     * The plugin uses a single eventfd as its poll descriptor.
     * snd_pcm_poll_descriptors_count() must return 1.
     */
    init_sock_path("poll-count");
    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);

    int count = snd_pcm_poll_descriptors_count(pcm);
    CHECK(count == 1);

    snd_pcm_close(pcm);
}

TEST(poll_revents_returns_pollout_when_eventfd_signalled)
{
    /*
     * The plugin signals the eventfd (POLLIN on the fd) to indicate that
     * write space is available.  snd_pcm_poll_descriptors_revents() must
     * translate POLLIN → POLLOUT for the ALSA layer.
     *
     * We get the poll fds, manually write to the eventfd, then call
     * snd_pcm_poll_descriptors_revents() and check for POLLOUT.
     */
    init_sock_path("poll-revt");
    server_args_t args = { .sock_path = g_sock_path, .response = PCDSP_ERR_OK };
    pthread_t srv = start_mock_server(&args);
    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);
    CHECK(set_hw_params(pcm) == 0);

    struct pollfd pfds[1];
    int count = snd_pcm_poll_descriptors(pcm, pfds, 1);
    CHECK(count == 1);
    CHECK(pfds[0].fd >= 0);
    CHECK(pfds[0].events == POLLIN);

    /* Signal the eventfd as the worker thread would */
    uint64_t val = 1;
    CHECK(write(pfds[0].fd, &val, sizeof(val)) == (ssize_t)sizeof(val));

    /* Poll with zero timeout (it should be readable immediately) */
    pfds[0].revents = 0;
    int nready = poll(pfds, 1, 0);
    CHECK(nready == 1);
    CHECK(pfds[0].revents & POLLIN);

    /* Let the plugin translate POLLIN → POLLOUT */
    unsigned short revents = 0;
    int rc = snd_pcm_poll_descriptors_revents(pcm, pfds, 1, &revents);
    CHECK(rc == 0);
    CHECK(revents & POLLOUT);

    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

/* -----------------------------------------------------------------------
 * Rapid format change (open/close + reopen with different format)
 * ---------------------------------------------------------------------- */

TEST(rapid_format_change_open_close_cycle)
{
    /*
     * Simulate rapid format changes: open the plugin, close it, reopen
     * with a different notional format.  No hw_params is called here
     * (no controller), so this only exercises the open/close lifecycle.
     *
     * The important property: no crash, no hang, no fd leak across cycles.
     */
    init_sock_path("rapid-fmt");
    for (int i = 0; i < 10; i++) {
        snd_pcm_t *pcm = NULL;
        int rc = open_plugin(&pcm, g_sock_path);
        CHECK(rc == 0);

        /* Poll count must be stable across cycles */
        int count = snd_pcm_poll_descriptors_count(pcm);
        CHECK(count == 1);

        snd_pcm_close(pcm);
    }
}

TEST(nonblocking_write_loop_completes_without_wait_timeout)
{
    /*
     * Regression for the real-runtime hang where playback reaches a state with
     * free ring-buffer space, but ALSA keeps timing out in poll() because the
     * plugin exposes only a one-shot eventfd edge. The mock controller keeps
     * the transferred pipe readable so the worker can make forward progress.
     */
    init_sock_path("write-loop");
    server_args_t args = {
        .sock_path  = g_sock_path,
        .response   = PCDSP_ERR_OK,
        .drain_pipe = true,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);
    CHECK(set_hw_params(pcm) == 0);
    CHECK(snd_pcm_nonblock(pcm, 1) == 0);

    int16_t frames[257 * 2] = { 0 };
    size_t written = 0;
    const size_t total_frames = 48000;

    while (written < total_frames) {
        snd_pcm_uframes_t chunk = (total_frames - written) > 257 ? 257 : (snd_pcm_uframes_t)(total_frames - written);
        snd_pcm_sframes_t rc = snd_pcm_writei(pcm, frames, chunk);
        if (rc == -EAGAIN) {
            int ready = snd_pcm_wait(pcm, 1000);
            CHECK(ready > 0);
            continue;
        }
        CHECK(rc >= 0);
        written += (size_t)rc;
    }

    CHECK(snd_pcm_nonblock(pcm, 0) == 0);
    CHECK(snd_pcm_drain(pcm) == 0);
    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

/* -----------------------------------------------------------------------
 * Drain timeout / pause synchronisation (Step 3 correctness fixes)
 * ---------------------------------------------------------------------- */

TEST(drain_times_out_when_camilladsp_stops_reading_pipe)
{
    /*
     * Regression: pcdsp_drain() previously waited for the ring buffer to
     * empty with no bound. If CamillaDSP is alive but stops reading stdin
     * (wedged) without ever closing its end (no -EPIPE), the kernel pipe
     * fills up and write() never sees POLLOUT again, so the ring buffer
     * never empties. snd_pcm_drain() must return -ETIMEDOUT once the
     * dynamic bound computed by pcdsp_drain_timeout_ns() elapses instead of
     * hanging forever.
     */
    init_sock_path("drain-timeout");
    _Atomic(bool) stop_holding = false;
    server_args_t args = {
        .sock_path         = g_sock_path,
        .response          = PCDSP_ERR_OK,
        .hold_pipe_no_read = true,
        .stop_holding      = &stop_holding,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);
    CHECK(set_hw_params(pcm) == 0);
    CHECK(snd_pcm_nonblock(pcm, 1) == 0);

    /* Feed enough frames to fill the ring buffer and the kernel pipe so the
     * worker is stuck retrying POLLOUT with nothing reading the other end. */
    int16_t frames[257 * 2] = { 0 };
    for (int i = 0; i < 64; i++) {
        snd_pcm_sframes_t rc = snd_pcm_writei(pcm, frames, 257);
        if (rc == -EAGAIN)
            break;
        CHECK(rc >= 0 || rc == -EAGAIN);
    }

    struct timespec drain_start, drain_end;
    clock_gettime(CLOCK_MONOTONIC, &drain_start);
    CHECK(snd_pcm_nonblock(pcm, 0) == 0);
    int drain_rc = snd_pcm_drain(pcm);
    clock_gettime(CLOCK_MONOTONIC, &drain_end);

    CHECK(drain_rc == -ETIMEDOUT);

    double elapsed_s = (double)(drain_end.tv_sec - drain_start.tv_sec) +
                        (double)(drain_end.tv_nsec - drain_start.tv_nsec) / 1e9;
    /* Bounded: strictly less than the server's max hold time, proving
     * drain() returned on its own timeout rather than the peer going away. */
    CHECK(elapsed_s < 7.0);

    atomic_store_explicit(&stop_holding, true, memory_order_release);
    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

TEST(drain_timeout_stops_worker_and_resets_state_to_setup)
{
    /*
     * Regression: matching BlueALSA's drain() error paths exactly —
     * bluealsa_drain() calls bluealsa_stop(io) and sets
     * io->state = SND_PCM_STATE_SETUP itself on timeout, rather than
     * relying on alsa-lib's generic post-drain auto-drop (confirmed via
     * pcm_ioplug.c: snd_pcm_ioplug_drain() only auto-drops when the
     * plugin's drain() callback returns 0). Previously pcdsp_drain()
     * returned -ETIMEDOUT but left the PCM in SND_PCM_STATE_DRAINING with
     * the worker thread still running (still retrying writes CamillaDSP
     * will never read). Verify the PCM is left in SND_PCM_STATE_SETUP
     * (not stuck in DRAINING) immediately after a timed-out drain, and
     * that the plugin can be started again from that state.
     */
    init_sock_path("drain-timeout-state");
    _Atomic(bool) stop_holding2 = false;
    server_args_t args2 = {
        .sock_path         = g_sock_path,
        .response          = PCDSP_ERR_OK,
        .hold_pipe_no_read = true,
        .stop_holding      = &stop_holding2,
    };
    pthread_t srv2 = start_mock_server(&args2);

    snd_pcm_t *pcm2 = NULL;
    CHECK(open_plugin(&pcm2, g_sock_path) == 0);
    CHECK(set_hw_params(pcm2) == 0);
    CHECK(snd_pcm_nonblock(pcm2, 1) == 0);

    int16_t frames2[257 * 2] = { 0 };
    for (int i = 0; i < 64; i++) {
        snd_pcm_sframes_t rc = snd_pcm_writei(pcm2, frames2, 257);
        if (rc == -EAGAIN)
            break;
        CHECK(rc >= 0 || rc == -EAGAIN);
    }

    CHECK(snd_pcm_nonblock(pcm2, 0) == 0);
    int drain_rc2 = snd_pcm_drain(pcm2);
    CHECK(drain_rc2 == -ETIMEDOUT);

    /* Must not be left stuck in DRAINING: the plugin itself transitions to
     * SETUP on a timed-out drain, matching BlueALSA exactly. */
    CHECK(snd_pcm_state(pcm2) == SND_PCM_STATE_SETUP);

    /* The PCM must still be usable afterward: a fresh prepare() should
     * succeed, proving the worker was actually stopped (not left spinning)
     * and can be restarted cleanly rather than the instance being wedged. */
    CHECK(snd_pcm_prepare(pcm2) == 0);
    CHECK(snd_pcm_state(pcm2) == SND_PCM_STATE_PREPARED);

    atomic_store_explicit(&stop_holding2, true, memory_order_release);
    snd_pcm_close(pcm2);
    pthread_join(srv2, NULL);
    unlink(g_sock_path);
}

/*
 * Helper for drain_timeout_scales_with_backlog_not_flat_constant: negotiate
 * hw_params with the given period, fill the pipeline, hold the peer's read
 * end open without reading, and return how long snd_pcm_drain() took to
 * give up with -ETIMEDOUT.
 */
static double measure_drain_timeout(const char *sock_suffix,
                                     snd_pcm_uframes_t period_hint,
                                     unsigned int rate)
{
    init_sock_path(sock_suffix);
    _Atomic(bool) stop_holding = false;
    server_args_t args = {
        .sock_path         = g_sock_path,
        .response          = PCDSP_ERR_OK,
        .hold_pipe_no_read = true,
        .stop_holding      = &stop_holding,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK_RET(open_plugin(&pcm, g_sock_path) == 0, -1.0);
    CHECK_RET(set_hw_params_ex(pcm, period_hint, rate) == 0, -1.0);
    CHECK_RET(snd_pcm_nonblock(pcm, 1) == 0, -1.0);

    int16_t frames[257 * 2] = { 0 };
    for (int i = 0; i < 256; i++) {
        snd_pcm_sframes_t rc = snd_pcm_writei(pcm, frames, 257);
        if (rc == -EAGAIN)
            break;
        CHECK_RET(rc >= 0 || rc == -EAGAIN, -1.0);
    }

    struct timespec drain_start, drain_end;
    clock_gettime(CLOCK_MONOTONIC, &drain_start);
    CHECK_RET(snd_pcm_nonblock(pcm, 0) == 0, -1.0);
    int drain_rc = snd_pcm_drain(pcm);
    clock_gettime(CLOCK_MONOTONIC, &drain_end);

    CHECK_RET(drain_rc == -ETIMEDOUT, -1.0);

    atomic_store_explicit(&stop_holding, true, memory_order_release);
    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);

    return (double)(drain_end.tv_sec - drain_start.tv_sec) +
           (double)(drain_end.tv_nsec - drain_start.tv_nsec) / 1e9;
}

TEST(drain_timeout_scales_with_backlog_not_flat_constant)
{
    /*
     * Regression: pcdsp_drain()'s bound used to be the single flat
     * PCDSP_DRAIN_TIMEOUT_NS (5 s) no matter how much audio was actually
     * left to drain. BlueALSA instead computes
     * `100ms + periods_remaining * period_time` so a small backlog gives up
     * quickly and a large backlog gets proportionally more time before
     * -ETIMEDOUT. Prove both properties on the picoredsp plugin itself
     * (not just by inspecting the formula): a tiny period/buffer times out
     * much faster than a large period/buffer, and neither takes anywhere
     * near the old flat 5 s ceiling.
     */
    double small_s = measure_drain_timeout("drain-scale-small", 64, 48000);
    double large_s = measure_drain_timeout("drain-scale-large", 8192, 48000);

    /* Both must have actually timed out (measure_drain_timeout returns -1.0
     * on any assertion failure along the way). */
    CHECK(small_s >= 0.0);
    CHECK(large_s >= 0.0);

    /* Neither should resemble the old flat 5 s constant. */
    CHECK(small_s < 1.0);
    CHECK(large_s < 5.0);

    /* The larger backlog must take meaningfully longer than the tiny one —
     * this is the actual BlueALSA-style scaling behaviour, not merely "some
     * bound exists". */
    CHECK(large_s > small_s * 2.0);
}

TEST(pause_blocks_until_worker_stops_writing_before_returning)
{
    /*
     * Regression: pcdsp_pause(enable=1) previously set an atomic flag and
     * returned immediately, racing the worker thread which could still be
     * mid-write to the pipe. It now blocks until the worker acknowledges
     * (via pause_mutex/pause_cond) that it has reached a safe point. Verify
     * this by checking that the mock CamillaDSP stops receiving bytes as
     * soon as pause() returns.
     */
    init_sock_path("pause-sync");
    _Atomic(long) bytes_read = 0;
    server_args_t args = {
        .sock_path  = g_sock_path,
        .response   = PCDSP_ERR_OK,
        .drain_pipe = true,
        .bytes_read = &bytes_read,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);
    CHECK(set_hw_params(pcm) == 0);
    CHECK(snd_pcm_nonblock(pcm, 1) == 0);

    /* Keep audio flowing continuously so the worker is actively writing
     * when pause() is invoked. */
    int16_t frames[257 * 2] = { 0 };
    size_t written = 0;
    const size_t total_frames = 48000; /* 1 second at 48 kHz */

    while (written < total_frames) {
        snd_pcm_uframes_t chunk = (total_frames - written) > 257
                                       ? 257
                                       : (snd_pcm_uframes_t)(total_frames - written);
        snd_pcm_sframes_t rc = snd_pcm_writei(pcm, frames, chunk);
        if (rc == -EAGAIN) {
            int ready = snd_pcm_wait(pcm, 1000);
            CHECK(ready > 0);
            continue;
        }
        CHECK(rc >= 0);
        written += (size_t)rc;
    }

    CHECK(snd_pcm_pause(pcm, 1) == 0);

    /* The pause() contract is "no further worker writes are in flight when it
     * returns." Bytes already queued in the kernel pipe before the worker
     * parked may still be drained by the mock reader briefly after return.
     * Wait for observed byte count to quiesce, then assert it stays stable. */
    long stable_bytes = -1;
    bool reached_stable = false;
    int stable_samples = 0;
    for (int i = 0; i < 50; i++) { /* up to 500 ms total */
        long before = atomic_load_explicit(&bytes_read, memory_order_relaxed);
        struct timespec step = { .tv_nsec = 10000000L }; /* 10 ms */
        nanosleep(&step, NULL);
        long after = atomic_load_explicit(&bytes_read, memory_order_relaxed);
        if (after == before) {
            stable_bytes = after;
            stable_samples++;
            if (stable_samples >= 3) {
                reached_stable = true;
                break;
            }
        } else {
            stable_samples = 0;
        }
    }
    CHECK(reached_stable);

    struct timespec verify = { .tv_nsec = 50000000L }; /* 50 ms */
    nanosleep(&verify, NULL);
    long bytes_after_verify = atomic_load_explicit(&bytes_read, memory_order_relaxed);
    CHECK(bytes_after_verify == stable_bytes);

    CHECK(snd_pcm_pause(pcm, 0) == 0);
    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

/* -----------------------------------------------------------------------
 * sw_params avail_min / delay() pipe accounting (Step 4 correctness fixes)
 * ---------------------------------------------------------------------- */

TEST(delay_accounts_for_frames_queued_in_kernel_pipe)
{
    /*
     * Regression: pcdsp_delay() previously only reported frames still
     * sitting in the plugin's ring buffer, silently losing track of frames
     * once the worker thread handed them off to the kernel pipe — real
     * audio that has not yet reached CamillaDSP/the DAC, but was reported
     * as if it had already arrived. Verify delay() now also counts
     * pipe-queued bytes (via FIONREAD) on top of the ring buffer.
     *
     * Uses the same shrunk-pipe/wedged-consumer setup as the drain timeout
     * test: the worker can hand at most ~1024 frames off to the 4 KiB pipe
     * before it fills, so once settled, some previously-written frames
     * live in the pipe rather than the ring buffer.
     */
    init_sock_path("delay-pipe");
    _Atomic(bool) stop_holding = false;
    server_args_t args = {
        .sock_path         = g_sock_path,
        .response          = PCDSP_ERR_OK,
        .hold_pipe_no_read = true,
        .stop_holding      = &stop_holding,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);
    CHECK(set_hw_params(pcm) == 0); /* period=1024, buffer_size=4096 frames */
    CHECK(snd_pcm_nonblock(pcm, 1) == 0);

    int16_t frames[257 * 2] = { 0 };
    snd_pcm_sframes_t total_written = 0;
    while (total_written < 4096) {
        snd_pcm_sframes_t rc = snd_pcm_writei(pcm, frames, 257);
        if (rc == -EAGAIN)
            break;
        CHECK(rc >= 0);
        total_written += rc;
    }
    CHECK(total_written > 0);

    if (snd_pcm_state(pcm) == SND_PCM_STATE_PREPARED)
        CHECK(snd_pcm_start(pcm) == 0);

    /* Let the worker hand off as many frames as the shrunk pipe can hold. */
    struct timespec settle = { .tv_nsec = 300000000L }; /* 300 ms */
    nanosleep(&settle, NULL);

    snd_pcm_sframes_t delay = -1;
    CHECK(snd_pcm_delay(pcm, &delay) == 0);
    /* Every frame written so far must be accounted for — whether still in
     * the ring buffer or already handed off to the pipe — proving frames
     * are not silently dropped from the delay estimate once they leave the
     * ring buffer. Prior to this fix, delay would fall roughly a whole
     * pipe-capacity short of total_written. Allow slack of one
     * PCDSP_PIPE_CHUNK_FRAMES: the worker pulls frames out of the ring
     * buffer in that granularity before writing them to the pipe, so at
     * most one in-flight chunk can transiently be counted by neither the
     * ring buffer nor FIONREAD (analogous to a small hardware FIFO) when
     * the peer stops reading mid-chunk. */
    CHECK(delay >= total_written - (snd_pcm_sframes_t)PCDSP_PIPE_CHUNK_FRAMES);

    atomic_store_explicit(&stop_holding, true, memory_order_release);
    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

TEST(poll_revents_respects_avail_min_from_sw_params)
{
    /*
     * Regression: poll_revents() previously reported readiness (POLLOUT) as
     * soon as a single ring-buffer frame was free, ignoring avail_min
     * negotiated via snd_pcm_sw_params(). Simulate a CamillaDSP that is
     * alive but not reading stdin (wedged) behind a deliberately shrunk
     * kernel pipe, so only a bounded amount of ring-buffer space can ever
     * free up (~ the pipe's frame capacity). With an avail_min this
     * scenario can never satisfy, snd_pcm_wait() must keep timing out
     * instead of falsely reporting readiness.
     */
    init_sock_path("avail-min");
    _Atomic(bool) stop_holding = false;
    server_args_t args = {
        .sock_path         = g_sock_path,
        .response          = PCDSP_ERR_OK,
        .hold_pipe_no_read = true,
        .stop_holding      = &stop_holding,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);
    CHECK(set_hw_params(pcm) == 0); /* period=1024, buffer_size=4096 frames */

    /* Negotiate an avail_min the shrunk-pipe scenario can never reach: the
     * worker can hand at most ~1024 frames off to the 4 KiB pipe before it
     * fills, so ring-buffer space will plateau well below buffer_size. */
    snd_pcm_sw_params_t *swparams;
    snd_pcm_sw_params_alloca(&swparams);
    CHECK(snd_pcm_sw_params_current(pcm, swparams) == 0);
    CHECK(snd_pcm_sw_params_set_avail_min(pcm, swparams, 4096) == 0);
    CHECK(snd_pcm_sw_params(pcm, swparams) == 0);

    CHECK(snd_pcm_nonblock(pcm, 1) == 0);
    int16_t frames[257 * 2] = { 0 };
    snd_pcm_sframes_t total_written = 0;
    while (total_written < 4096) {
        snd_pcm_sframes_t rc = snd_pcm_writei(pcm, frames, 257);
        if (rc == -EAGAIN)
            break;
        CHECK(rc >= 0);
        total_written += rc;
    }
    CHECK(total_written > 0);

    if (snd_pcm_state(pcm) == SND_PCM_STATE_PREPARED)
        CHECK(snd_pcm_start(pcm) == 0);

    /* Let the worker hand off everything the shrunk pipe can absorb. */
    struct timespec settle = { .tv_nsec = 300000000L }; /* 300 ms */
    nanosleep(&settle, NULL);

    /* Even though *some* space has freed up (the pipe's ~1024-frame
     * capacity), it never reaches avail_min (4096): poll must never report
     * readiness. */
    for (int i = 0; i < 3; i++) {
        int ready = snd_pcm_wait(pcm, 100);
        CHECK(ready == 0); /* 0 == timed out, no revents */
    }

    atomic_store_explicit(&stop_holding, true, memory_order_release);
    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

/* -----------------------------------------------------------------------
 * Device disconnect (BlueALSA-parity: proactive DISCONNECTED state)
 * ---------------------------------------------------------------------- */

TEST(pointer_and_poll_report_disconnected_after_camilladsp_exits)
{
    /*
     * Regression: previously, once the worker recorded a fatal stream
     * error (CamillaDSP exited, kernel closed the pipe read end so the
     * next write got -EPIPE), pcdsp_pointer() returned that negative errno
     * directly. alsa-lib's snd_pcm_ioplug_hw_ptr_update() treats any
     * negative pointer() return as XRUN, never as DISCONNECTED — so the
     * application could never distinguish "CamillaDSP is permanently gone,
     * close and reopen" from an ordinary, recoverable XRUN. Matching
     * BlueALSA's bluealsa_pointer()/poll_revents() exactly: the plugin must
     * now call snd_pcm_ioplug_set_state(io, SND_PCM_STATE_DISCONNECTED)
     * itself and report -ENODEV, not a bare XRUN.
     */
    init_sock_path("disconnect");
    server_args_t args = {
        .sock_path              = g_sock_path,
        .response               = PCDSP_ERR_OK,
        .close_pipe_after_bytes = 512,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);
    CHECK(set_hw_params(pcm) == 0); /* period=1024, buffer_size=4096 frames */
    CHECK(snd_pcm_nonblock(pcm, 1) == 0);

    int16_t frames[257 * 2] = { 0 };

    /* Keep writing (and letting the worker hand frames to the pipe) until
     * the plugin observes the pipe close and reports it, or a generous
     * bound elapses. Every write is deliberately small (257 frames) and
     * spaced out so the worker has time to actually flush each chunk to
     * the pipe and hit -EPIPE once the mock server closes its end after
     * 512 bytes, rather than depending on a single oversized write. */
    bool became_disconnected = false;
    for (int i = 0; i < 200 && !became_disconnected; i++) {
        snd_pcm_writei(pcm, frames, 257); /* ignore rc: EAGAIN/EPIPE both fine here */
        struct timespec step = { .tv_nsec = 10000000L }; /* 10 ms */
        nanosleep(&step, NULL);
        if (snd_pcm_state(pcm) == SND_PCM_STATE_DISCONNECTED)
            became_disconnected = true;
    }

    CHECK(became_disconnected);
    CHECK(snd_pcm_state(pcm) == SND_PCM_STATE_DISCONNECTED);

    /* Matching BlueALSA's contract: once DISCONNECTED, further writes must
     * report -ENODEV (a permanent, non-recoverable error), not merely
     * XRUN, so the application knows to close() and reopen rather than
     * call snd_pcm_prepare() and retry. */
    snd_pcm_sframes_t rc = snd_pcm_writei(pcm, frames, 257);
    CHECK(rc == -ENODEV);

    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

TEST(prepare_refuses_to_silently_clear_a_fatal_stream_error)
{
    /*
     * Regression: pcdsp_prepare() previously reset stream_error to 0
     * unconditionally, so an application that called snd_pcm_prepare()
     * after a fatal error (e.g. as part of ordinary XRUN recovery) would
     * be told the stream is healthy again — while writes kept hitting the
     * same broken pipe_fd (IPC/pipe re-connection only happens in
     * hw_params(), not prepare()), immediately re-failing. Matching
     * BlueALSA's bluealsa_prepare(): once disconnected, prepare() itself
     * must also report the disconnect rather than papering over it.
     */
    init_sock_path("disconnect-prepare");
    server_args_t args = {
        .sock_path              = g_sock_path,
        .response               = PCDSP_ERR_OK,
        .close_pipe_after_bytes = 512,
    };
    pthread_t srv = start_mock_server(&args);

    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);
    CHECK(set_hw_params(pcm) == 0);
    CHECK(snd_pcm_nonblock(pcm, 1) == 0);

    int16_t frames[257 * 2] = { 0 };
    bool became_disconnected = false;
    for (int i = 0; i < 200 && !became_disconnected; i++) {
        snd_pcm_writei(pcm, frames, 257);
        struct timespec step = { .tv_nsec = 10000000L }; /* 10 ms */
        nanosleep(&step, NULL);
        if (snd_pcm_state(pcm) == SND_PCM_STATE_DISCONNECTED)
            became_disconnected = true;
    }
    CHECK(became_disconnected);

    /* prepare() must refuse (report the disconnect), not silently reset
     * the PCM to a state that looks usable. */
    CHECK(snd_pcm_prepare(pcm) == -ENODEV);
    CHECK(snd_pcm_state(pcm) == SND_PCM_STATE_DISCONNECTED);

    snd_pcm_close(pcm);
    pthread_join(srv, NULL);
    unlink(g_sock_path);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("test_pcm_integration\n");

    /* open / close */
    RUN(plugin_opens_and_closes_without_controller);
    RUN(rapid_open_close_does_not_leak_fds);

    /* hw_params */
    RUN(hw_params_fails_when_no_controller_present);
    RUN(hw_params_succeeds_with_mock_controller_and_ready);
    RUN(hw_params_fails_with_error_config_from_controller);
    RUN(hw_params_fails_with_error_playback_device_from_controller);

    /* Unsupported format */
    RUN(hw_params_fails_for_unsupported_format);

    /* Poll */
    RUN(poll_descriptors_count_is_one);
    RUN(poll_revents_returns_pollout_when_eventfd_signalled);

    /* Rapid format change */
    RUN(rapid_format_change_open_close_cycle);

    /* Playback completion regression */
    RUN(nonblocking_write_loop_completes_without_wait_timeout);

    /* Drain timeout / pause synchronisation (Step 3) */
    RUN(drain_times_out_when_camilladsp_stops_reading_pipe);
    RUN(drain_timeout_stops_worker_and_resets_state_to_setup);
    RUN(drain_timeout_scales_with_backlog_not_flat_constant);
    RUN(pause_blocks_until_worker_stops_writing_before_returning);

    /* sw_params avail_min / delay() pipe accounting (Step 4) */
    RUN(delay_accounts_for_frames_queued_in_kernel_pipe);
    RUN(poll_revents_respects_avail_min_from_sw_params);

    /* device disconnect (BlueALSA-parity proactive DISCONNECTED state) */
    RUN(pointer_and_poll_report_disconnected_after_camilladsp_exits);
    RUN(prepare_refuses_to_silently_clear_a_fatal_stream_error);

    printf("\n%d passed, %d failed\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
}
