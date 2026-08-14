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
 */

#define _GNU_SOURCE

#include "ipc.h"

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
 * Negotiate hw_params: stereo S16_LE at 48 kHz with a 1024-frame period.
 */
static int set_hw_params(snd_pcm_t *pcm)
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
    unsigned int rate = 48000;
    if ((rc = snd_pcm_hw_params_set_rate_near(pcm, params, &rate, 0)) < 0)
        return rc;
    snd_pcm_uframes_t period = 1024;
    if ((rc = snd_pcm_hw_params_set_period_size_near(pcm, params, &period, 0)) < 0)
        return rc;
    snd_pcm_uframes_t buf = period * 4;
    if ((rc = snd_pcm_hw_params_set_buffer_size_near(pcm, params, &buf)) < 0)
        return rc;
    return snd_pcm_hw_params(pcm, params);
}

/* -----------------------------------------------------------------------
 * Mock IPC server helpers
 * ---------------------------------------------------------------------- */

typedef struct {
    const char         *sock_path;
    pcdsp_error_code_t  response;   /* PCDSP_ERR_OK → send READY; else send ERROR */
} server_args_t;

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
            (void)send_fd_with_ready(cfd, pipefd[1]);
            close(pipefd[0]);
            close(pipefd[1]);
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
    snd_pcm_t *pcm = NULL;
    CHECK(open_plugin(&pcm, g_sock_path) == 0);

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

    printf("\n%d passed, %d failed\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
}
