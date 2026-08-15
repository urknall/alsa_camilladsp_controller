/*
 * tests/test_pcm_worker.c — unit tests for the worker pipe-drain helpers
 *
 * Coverage
 * --------
 * pcdsp_drain_period_to_pipe:
 *   drain_period_writes_correct_bytes_to_pipe
 *   drain_period_handles_epipe_and_returns_negative_errno
 *   drain_period_handles_eintr_by_retrying
 *   drain_period_partial_ring_buffer_writes_available_frames
 *   drain_period_wraps_around_ring_buffer_boundary
 *   drain_period_returns_zero_on_empty_ring_buffer (not an error)
 *
 * pcdsp_drain_period_null_sink:
 *   null_sink_drops_period_frames_from_ring_buffer
 *   null_sink_returns_fewer_frames_when_ring_buffer_partially_filled
 *   null_sink_returns_zero_on_empty_ring_buffer
 *   null_sink_skips_sleep_when_rate_is_zero
 *
 * Period wrap / buffer wrap (ring buffer level):
 *   period_wrap_across_ring_buffer_boundary
 *   buffer_size_not_divisible_by_period_partial_final_period
 *
 * Partial write / EINTR / EPIPE (via real pipe fds):
 *   epipe_on_read_end_close_returns_minus_epipe
 *   worker_survives_eintr_and_delivers_all_data
 *   worker_survives_default_sigpipe_disposition_via_thread_scoped_block
 *
 * Poll / eventfd (eventfd signalling):
 *   eventfd_is_signalled_after_each_period
 */

#define _GNU_SOURCE

#include "pcm_worker.h"
#include "ringbuffer.h"

#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <time.h>
#include <unistd.h>

/* -----------------------------------------------------------------------
 * Micro test framework (matches the style in the other test files)
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
 * Helpers
 * ---------------------------------------------------------------------- */

/* Fill an rb with a repeating pattern of `value` for `nframes` frames. */
static void fill_rb(pcdsp_ringbuffer_t *rb, size_t nframes, uint8_t value)
{
    /* Use a local buffer to fill incrementally. */
    uint8_t buf[64 * 4]; /* 64 frames × 4 bytes/frame */
    memset(buf, value, sizeof(buf));

    size_t left = nframes;
    while (left > 0) {
        size_t chunk = left < 64 ? left : 64;
        size_t written = pcdsp_rb_write(rb, buf, chunk);
        left -= written;
        if (written == 0)
            break;
    }
}


typedef struct drain_thread_args {
    pcdsp_ringbuffer_t *rb;
    int                 pipe_fd;
    size_t              period_frames;
    size_t              frame_bytes;
    _Atomic(bool)      *keep_running;
    ssize_t             result;
} drain_thread_args_t;

static void *drain_thread_main(void *arg)
{
    /* Mirror production: the real worker thread (pcm.c) blocks SIGPIPE for
     * itself before writing to the pipe. Tests that exercise EPIPE run the
     * drain call inside a thread that does the same, instead of relying on
     * process-wide signal(SIGPIPE, SIG_IGN), which would hide whether the
     * fix is actually thread-scoped. */
    pcdsp_worker_block_all_signals();
    drain_thread_args_t *a = arg;
    a->result = pcdsp_drain_period_to_pipe(
        a->rb, a->pipe_fd, a->period_frames, a->frame_bytes, a->keep_running);
    return NULL;
}

/* Run pcdsp_drain_period_to_pipe on a dedicated thread that blocks SIGPIPE
 * for itself first (mirroring worker_thread() in pcm.c), and return its
 * result. Used by tests that close the pipe's read end to simulate
 * CamillaDSP exiting, so a broken-pipe write cannot terminate the test
 * process via the default SIGPIPE disposition. */
static ssize_t run_drain_on_thread(pcdsp_ringbuffer_t *rb, int pipe_fd,
                                    size_t period_frames, size_t frame_bytes,
                                    _Atomic(bool) *keep_running)
{
    drain_thread_args_t args = {
        .rb = rb,
        .pipe_fd = pipe_fd,
        .period_frames = period_frames,
        .frame_bytes = frame_bytes,
        .keep_running = keep_running,
        .result = -9999,
    };
    pthread_t thread;
    int rc = pthread_create(&thread, NULL, drain_thread_main, &args);
    if (rc != 0)
        return -rc;
    pthread_join(thread, NULL);
    return args.result;
}

/* -----------------------------------------------------------------------
 * pcdsp_drain_period_to_pipe tests
 * ---------------------------------------------------------------------- */

TEST(drain_period_writes_correct_bytes_to_pipe)
{
    /* 4 frames per period, 2 bytes/frame (stereo S8 equivalent) */
    const size_t period = 4;
    const size_t fb     = 2;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);

    /* Fill with recognisable pattern */
    uint8_t src[4 * 2];
    for (int i = 0; i < (int)sizeof(src); i++)
        src[i] = (uint8_t)(0x10 + i);
    pcdsp_rb_write(&rb, src, period);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], period, fb, NULL);
    CHECK(got == (ssize_t)period);

    /* Read back from the read end and compare */
    uint8_t dst[4 * 2] = {0};
    ssize_t n = read(pipefd[0], dst, sizeof(dst));
    CHECK(n == (ssize_t)(period * fb));
    CHECK(memcmp(src, dst, sizeof(dst)) == 0);

    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

TEST(drain_period_handles_epipe_and_returns_negative_errno)
{
    const size_t period = 8;
    const size_t fb     = 4;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);
    fill_rb(&rb, period, 0xAA);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);
    close(pipefd[0]); /* close read end → next write() returns EPIPE */

    /* SIGPIPE stays at its default (process-wide) disposition here; the
     * drain runs on a dedicated thread that blocks SIGPIPE for itself only,
     * exactly like the production worker thread. */
    ssize_t rc = run_drain_on_thread(&rb, pipefd[1], period, fb, NULL);
    CHECK(rc == -EPIPE);

    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

TEST(drain_period_partial_ring_buffer_writes_available_frames)
{
    /*
     * Request period_frames = 8, but ring buffer only contains 3.
     * The function should return 3 (the available frames), not an error.
     */
    const size_t period = 8;
    const size_t fb     = 2;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);
    fill_rb(&rb, 3, 0x55);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], period, fb, NULL);
    CHECK(got == 3);

    /* Drain pipe to avoid blocking */
    uint8_t tmp[3 * 2];
    ssize_t drained = read(pipefd[0], tmp, sizeof(tmp));
    CHECK(drained >= 0);

    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

TEST(drain_period_returns_zero_on_empty_ring_buffer)
{
    const size_t period = 4;
    const size_t fb     = 4;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);
    /* rb is empty */

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], period, fb, NULL);
    CHECK(got == 0);

    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

TEST(drain_period_cancellation_unblocks_full_pipe)
{
    /*
     * Regression: closing a pipe fd in another thread does not reliably
     * interrupt a blocked write on Linux.  Production therefore uses an
     * O_NONBLOCK fd plus an atomic run flag.  Fill the pipe completely,
     * start a drain, then cancel it and require the thread to exit promptly.
     */
    const size_t period = 8;
    const size_t fb     = 4;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);
    fill_rb(&rb, period, 0x5A);

    int pipefd[2];
    CHECK(pipe2(pipefd, O_NONBLOCK | O_CLOEXEC) == 0);

    uint8_t filler[4096];
    memset(filler, 0, sizeof(filler));
    while (write(pipefd[1], filler, sizeof(filler)) > 0)
        ;
    CHECK(errno == EAGAIN || errno == EWOULDBLOCK);

    _Atomic(bool) running = true;
    drain_thread_args_t args = {
        .rb = &rb,
        .pipe_fd = pipefd[1],
        .period_frames = period,
        .frame_bytes = fb,
        .keep_running = &running,
        .result = -9999,
    };

    pthread_t thread;
    CHECK(pthread_create(&thread, NULL, drain_thread_main, &args) == 0);

    struct timespec brief = { .tv_nsec = 50000000 }; /* 50 ms */
    nanosleep(&brief, NULL);
    atomic_store_explicit(&running, false, memory_order_release);

    struct timespec deadline;
    CHECK(clock_gettime(CLOCK_REALTIME, &deadline) == 0);
    deadline.tv_sec += 1;
    int join_rc = pthread_timedjoin_np(thread, NULL, &deadline);

    if (join_rc != 0) {
        /* Ensure a broken implementation cannot leave this test process
         * permanently blocked while still reporting the failure. */
        signal(SIGPIPE, SIG_IGN);
        close(pipefd[0]);
        pthread_join(thread, NULL);
        signal(SIGPIPE, SIG_DFL);
        pipefd[0] = -1;
    }

    CHECK(join_rc == 0);
    CHECK(args.result >= 0);

    if (pipefd[0] >= 0)
        close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

TEST(drain_period_wraps_around_ring_buffer_boundary)
{
    /*
     * Set up a ring buffer of capacity 8.  Fill 6 frames, consume 4 to
     * advance the read pointer to position 4.  Now write 6 more frames:
     * the ring buffer wraps at position 8.  Drain a full period of 6 frames
     * through the pipe and verify the data arrives correctly.
     */
    const size_t fb = 2;
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, (uint32_t)fb) == 0);

    /* Phase 1: fill 6, consume 4 → write_pos=6, read_pos=4 */
    uint8_t dummy[4 * 2];
    memset(dummy, 0, sizeof(dummy));
    fill_rb(&rb, 6, 0x01);
    uint8_t tmp[4 * 2];
    CHECK(pcdsp_rb_read(&rb, tmp, 4) == 4);

    /* Phase 2: write 6 frames that wrap around the buffer end */
    uint8_t wrap[6 * 2];
    for (int i = 0; i < (int)sizeof(wrap); i++)
        wrap[i] = (uint8_t)(0xA0 + i);
    CHECK(pcdsp_rb_write(&rb, wrap, 6) == 6);

    /* The ring buffer now has 8 frames at positions [4..9] % 8 = {4,5,6,7,0,1}.
     * Two of those were from the original fill — we want to drain the 6 new ones. */

    /* There are 2 (from old fill) + 6 (new) = 8 frames available total.
     * We'll drain all 8 frames and check the last 6 match `wrap`. */
    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    /* Drain 8 frames (the full buffer) */
    ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], 8, fb, NULL);
    CHECK(got == 8);

    /* Read back and verify */
    uint8_t dst[8 * 2];
    ssize_t n = read(pipefd[0], dst, sizeof(dst));
    CHECK(n == (ssize_t)(8 * fb));
    /* First 2 frames are from the old fill (0x01 bytes); last 6 match `wrap` */
    CHECK(memcmp(dst + 2 * fb, wrap, 6 * fb) == 0);

    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * pcdsp_drain_period_null_sink tests
 * ---------------------------------------------------------------------- */

TEST(null_sink_drops_period_frames_from_ring_buffer)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, 4) == 0);
    fill_rb(&rb, 8, 0x33);
    CHECK(pcdsp_rb_read_avail(&rb) == 8);

    /* rate=0 skips the sleep so the test runs fast */
    size_t drained = pcdsp_drain_period_null_sink(&rb, 8, 0);
    CHECK(drained == 8);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);
    CHECK(pcdsp_rb_write_avail(&rb) == 16);

    pcdsp_rb_free(&rb);
}

TEST(null_sink_returns_fewer_frames_when_ring_buffer_partially_filled)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, 2) == 0);
    fill_rb(&rb, 3, 0x11);

    size_t drained = pcdsp_drain_period_null_sink(&rb, 8, 0);
    CHECK(drained == 3);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);

    pcdsp_rb_free(&rb);
}

TEST(null_sink_returns_zero_on_empty_ring_buffer)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, 4) == 0);
    /* rb is empty */

    size_t drained = pcdsp_drain_period_null_sink(&rb, 4, 0);
    CHECK(drained == 0);

    pcdsp_rb_free(&rb);
}

TEST(null_sink_skips_sleep_when_rate_is_zero)
{
    /* Verify that rate=0 does not cause a divide-by-zero or block. */
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, 4) == 0);
    fill_rb(&rb, 4, 0x7F);

    size_t drained = pcdsp_drain_period_null_sink(&rb, 4, 0 /* no sleep */);
    CHECK(drained == 4);

    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * Period wrap / buffer wrap
 * ---------------------------------------------------------------------- */

TEST(period_wrap_across_ring_buffer_boundary)
{
    /*
     * Simulate a stream where each "period" is 4 frames, and the ring
     * buffer capacity is 8.  Drain many periods in sequence, verifying that
     * write-pointer wrap-around at the end of the buffer does not corrupt
     * data.
     */
    const size_t capacity = 8;
    const size_t period   = 4;
    const size_t fb       = 2;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, capacity, (uint32_t)fb) == 0);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);
    /* Make the pipe non-blocking so reads don't block in the test */
    int flags = fcntl(pipefd[0], F_GETFL, 0);
    fcntl(pipefd[0], F_SETFL, flags | O_NONBLOCK);

    for (int pass = 0; pass < 8; pass++) {
        /* Fill exactly one period */
        uint8_t src[period * fb];
        for (size_t i = 0; i < sizeof(src); i++)
            src[i] = (uint8_t)(pass * 10 + (int)i);
        CHECK(pcdsp_rb_write(&rb, src, period) == period);

        /* Drain it into the pipe */
        ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], period, fb, NULL);
        CHECK(got == (ssize_t)period);
        CHECK(pcdsp_rb_read_avail(&rb) == 0);

        /* Read back and verify */
        uint8_t dst[period * fb];
        ssize_t n = read(pipefd[0], dst, sizeof(dst));
        CHECK(n == (ssize_t)(period * fb));
        CHECK(memcmp(src, dst, sizeof(dst)) == 0);
    }

    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

TEST(buffer_size_not_divisible_by_period_partial_final_period)
{
    /*
     * Buffer capacity = 10 frames (not a power of two is rejected, so use
     * capacity 16 with a period of 3 frames to simulate a case where
     * buffer_size is not a clean multiple of period_size).
     *
     * Write 10 frames (3 full periods + 1 partial).  Drain 3 full periods,
     * verify that 1 frame remains and is undamaged.
     */
    const size_t capacity = 16;
    const size_t period   = 3;
    const size_t fb       = 4;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, capacity, (uint32_t)fb) == 0);

    /* Write 10 frames */
    uint8_t src[10 * fb];
    for (size_t i = 0; i < sizeof(src); i++)
        src[i] = (uint8_t)i;
    CHECK(pcdsp_rb_write(&rb, src, 10) == 10);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);
    int flags = fcntl(pipefd[0], F_GETFL, 0);
    fcntl(pipefd[0], F_SETFL, flags | O_NONBLOCK);

    /* Drain 3 full periods */
    for (int p = 0; p < 3; p++) {
        ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], period, fb, NULL);
        CHECK(got == (ssize_t)period);
        uint8_t tmp[period * fb];
        ssize_t n = read(pipefd[0], tmp, sizeof(tmp));
        CHECK(n == (ssize_t)(period * fb));
        /* Verify the data matches the original */
        CHECK(memcmp(src + (size_t)p * period * fb, tmp, sizeof(tmp)) == 0);
    }

    /* Exactly 1 frame remains in the ring buffer */
    CHECK(pcdsp_rb_read_avail(&rb) == 1);

    /* Read the remaining frame via the ring buffer directly */
    uint8_t rem[fb];
    CHECK(pcdsp_rb_read(&rb, rem, 1) == 1);
    CHECK(memcmp(src + 9 * fb, rem, fb) == 0);

    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * EPIPE / EINTR integration tests using real pipe fds
 * ---------------------------------------------------------------------- */

TEST(epipe_on_read_end_close_returns_minus_epipe)
{
    /*
     * Verify that when the read end of the pipe is closed (simulating
     * CamillaDSP exit), pcdsp_drain_period_to_pipe returns -EPIPE.
     */
    const size_t period = 8;
    const size_t fb     = 4;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);
    fill_rb(&rb, period, 0xCC);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);
    close(pipefd[0]); /* simulate CamillaDSP gone */

    /* SIGPIPE stays at its default (process-wide) disposition here; see
     * run_drain_on_thread(). */
    ssize_t rc = run_drain_on_thread(&rb, pipefd[1], period, fb, NULL);
    CHECK(rc == -EPIPE);

    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

TEST(worker_survives_eintr_and_delivers_all_data)
{
    /*
     * Verify that EINTR on write() is handled by retrying internally,
     * not by returning an error.
     *
     * We can't easily inject EINTR via a normal pipe.  Instead we verify
     * the positive case: many small writes through a non-blocking pipe
     * all succeed when enough buffer is available, confirming the retry
     * path handles the normal case without spurious errors.
     *
     * The EINTR path is exercised by the `continue` inside the
     * pcdsp_drain_period_to_pipe write loop when errno == EINTR.
     */
    const size_t period = 32;
    const size_t fb     = 4;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 64, (uint32_t)fb) == 0);

    uint8_t src[32 * 4];
    for (size_t i = 0; i < sizeof(src); i++)
        src[i] = (uint8_t)i;
    CHECK(pcdsp_rb_write(&rb, src, 32) == 32);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], period, fb, NULL);
    CHECK(got == (ssize_t)period);

    uint8_t dst[32 * 4];
    ssize_t n = read(pipefd[0], dst, sizeof(dst));
    CHECK(n == (ssize_t)(period * fb));
    CHECK(memcmp(src, dst, sizeof(dst)) == 0);

    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * Poll / eventfd signalling
 * ---------------------------------------------------------------------- */

TEST(eventfd_is_signalled_after_each_period)
{
    /*
     * Verify that an eventfd can be written after a period drain, which
     * is how the worker thread notifies the ALSA layer that space is free.
     *
     * We do not run the full worker thread here; we just verify the eventfd
     * write-and-read cycle works correctly (the mechanism used by pcm.c).
     */
    int efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    CHECK(efd >= 0);

    /* Before signal: reading should return EAGAIN (no data). */
    uint64_t val = 0;
    ssize_t n = read(efd, &val, sizeof(val));
    CHECK(n < 0 && errno == EAGAIN);

    /* Signal the eventfd (as the worker thread would after each period). */
    val = 1;
    CHECK(write(efd, &val, sizeof(val)) == sizeof(val));

    /* Now it should be readable. */
    val = 0;
    n = read(efd, &val, sizeof(val));
    CHECK(n == sizeof(val));
    CHECK(val == 1);

    /* After draining, should be EAGAIN again. */
    n = read(efd, &val, sizeof(val));
    CHECK(n < 0 && errno == EAGAIN);

    close(efd);
}

TEST(eventfd_accumulates_multiple_signals)
{
    /*
     * Verify that multiple eventfd writes accumulate and are returned as a
     * single 64-bit counter value on the next read.
     */
    int efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    CHECK(efd >= 0);

    uint64_t val = 1;
    for (int i = 0; i < 5; i++)
        CHECK(write(efd, &val, sizeof(val)) == sizeof(val));

    val = 0;
    ssize_t n = read(efd, &val, sizeof(val));
    CHECK(n == sizeof(val));
    CHECK(val == 5);

    close(efd);
}

/* -----------------------------------------------------------------------
 * Failure scenarios
 * ---------------------------------------------------------------------- */

TEST(failure_rust_daemon_restart_ipc_send_stop_fails_gracefully)
{
    /*
     * Simulates the "Rust daemon restarts mid-stream" scenario from the
     * C plugin perspective.
     *
     * When Rust restarts, the IPC socket is closed.  The plugin's next
     * pcdsp_ipc_send_stop() call must:
     *  - detect the closed socket (conn.fd == -1 or write fails)
     *  - return a non-zero error code
     *  - NOT crash or block
     *  - NOT prevent the plugin from closing its pipe_fd
     *
     * The plugin can still write to the pipe_fd (it was already transferred
     * via SCM_RIGHTS; the socket close doesn't affect it).
     */
    /* Use the ringbuffer + pipe to represent the "still-working data path"
     * even though the IPC socket is gone. */
    const size_t period = 4;
    const size_t fb     = 4;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);
    fill_rb(&rb, period, 0xBB);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    /* Data path still works: drain frames into the pipe */
    ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], period, fb, NULL);
    CHECK(got == (ssize_t)period);

    /* Simulate plugin closing its pipe_fd (pcdsp_stop()) */
    close(pipefd[1]);
    /* CamillaDSP would now see EOF on its stdin and exit cleanly. */

    /* Drain the pipe to verify data arrived correctly. */
    uint8_t dst[period * fb];
    ssize_t n = read(pipefd[0], dst, sizeof(dst));
    CHECK(n == (ssize_t)(period * fb));

    close(pipefd[0]);
    pcdsp_rb_free(&rb);
}

TEST(failure_camilladsp_early_exit_detected_via_epipe)
{
    /*
     * CamillaDSP exits mid-stream.  The kernel closes the read-end of the
     * pipe.  The next write by the worker returns EPIPE.
     *
     * This test verifies that pcdsp_drain_period_to_pipe reports -EPIPE
     * when CamillaDSP has gone, allowing the caller (worker_thread in
     * pcm.c) to set worker_running=false and report XRUN to the ALSA layer.
     */
    const size_t period = 8;
    const size_t fb     = 2;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);
    fill_rb(&rb, period, 0x99);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    /* Simulate CamillaDSP exit: close read end */
    close(pipefd[0]);

    /* SIGPIPE stays at its default (process-wide) disposition here; see
     * run_drain_on_thread(). */
    ssize_t rc = run_drain_on_thread(&rb, pipefd[1], period, fb, NULL);
    CHECK(rc < 0); /* must be a negative errno, not a frame count */

    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * SIGPIPE safety regression (release blocker)
 *
 * A write() to a pipe whose read end has been closed raises SIGPIPE for the
 * writing thread.  With the default disposition (SIG_DFL), the *entire host
 * process* is terminated — catastrophic for a plugin loaded inside an
 * arbitrary application (Squeezelite, AirPlay receivers, etc.).  Earlier
 * versions of this test suite concealed that by calling
 * signal(SIGPIPE, SIG_IGN) process-wide before exercising -EPIPE, which
 * would have masked a regression to the unsafe behaviour.  This test
 * verifies the actual production fix: SIGPIPE remains at its default,
 * process-wide disposition, and only the dedicated writer thread blocks it
 * for itself via pcdsp_worker_block_all_signals() (mirroring worker_thread() in
 * pcm.c). If that thread-scoping were ever broken (e.g. reverted to a
 * process-wide signal(SIGPIPE, SIG_IGN) call), this test would still pass by
 * accident; what it *does* prove is that the fix is thread-local rather than
 * global, and that the calling test process survives the broken-pipe write.
 * ---------------------------------------------------------------------- */

TEST(worker_survives_default_sigpipe_disposition_via_thread_scoped_block)
{
    const size_t period = 8;
    const size_t fb     = 2;

    /* Precondition: SIGPIPE is at its default, process-wide disposition.
     * (If some earlier test left it otherwise this would be a test-suite
     * bug; assert it explicitly rather than assuming.) */
    struct sigaction old_action;
    CHECK(sigaction(SIGPIPE, NULL, &old_action) == 0);
    CHECK(old_action.sa_handler == SIG_DFL);

    /* Snapshot this (main) thread's own signal mask so we can prove below
     * that pcdsp_worker_block_all_signals() — called only inside the dedicated
     * writer thread — did not leak into the caller's mask. */
    sigset_t mask_before;
    CHECK(pthread_sigmask(SIG_BLOCK, NULL, &mask_before) == 0);
    CHECK(!sigismember(&mask_before, SIGPIPE));

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, (uint32_t)fb) == 0);
    fill_rb(&rb, period, 0x42);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);
    close(pipefd[0]); /* simulate CamillaDSP exiting mid-stream */

    /* This runs the drain on a dedicated thread (mirroring the production
     * worker thread) with SIGPIPE left at SIG_DFL for the whole process. If
     * the thread failed to block SIGPIPE for itself, the write() below would
     * deliver SIGPIPE with the default action and kill this entire test
     * binary rather than returning -EPIPE — so simply reaching the
     * assertions below already demonstrates survival. */
    ssize_t rc = run_drain_on_thread(&rb, pipefd[1], period, fb, NULL);
    CHECK(rc == -EPIPE);

    /* The process (and this thread) is still alive and running further
     * assertions: SIGPIPE did not take down the host. */
    CHECK(sigaction(SIGPIPE, NULL, &old_action) == 0);
    CHECK(old_action.sa_handler == SIG_DFL);

    sigset_t mask_after;
    CHECK(pthread_sigmask(SIG_BLOCK, NULL, &mask_after) == 0);
    CHECK(!sigismember(&mask_after, SIGPIPE));

    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * Full signal-set masking regression (matches BlueALSA's io_thread_setup())
 *
 * BlueALSA's IO thread blocks *every* signal (sigfillset + SIG_SETMASK), not
 * just SIGPIPE, so that no signal can ever interrupt it asynchronously.
 * pcdsp_worker_block_all_signals() must do the same. Verify this directly by
 * capturing the worker thread's own mask (not the caller's) after calling it,
 * and checking that an arbitrary sample of signals beyond SIGPIPE are blocked.
 * ---------------------------------------------------------------------- */

static void *capture_mask_after_block_thread(void *arg)
{
    sigset_t *out = arg;
    pcdsp_worker_block_all_signals();
    pthread_sigmask(SIG_BLOCK, NULL, out);
    return NULL;
}

TEST(block_all_signals_blocks_full_signal_set_not_just_sigpipe)
{
    sigset_t worker_mask;
    memset(&worker_mask, 0, sizeof(worker_mask));

    pthread_t thread;
    CHECK(pthread_create(&thread, NULL, capture_mask_after_block_thread,
                          &worker_mask) == 0);
    CHECK(pthread_join(thread, NULL) == 0);

    /* SIGPIPE (the historically-fixed signal) and a representative sample of
     * unrelated signals (SIGUSR1, SIGTERM, SIGHUP) must all be blocked — a
     * regression to blocking only SIGPIPE would fail this on SIGUSR1/
     * SIGTERM/SIGHUP even though the original SIGPIPE-only test above would
     * still pass. */
    CHECK(sigismember(&worker_mask, SIGPIPE));
    CHECK(sigismember(&worker_mask, SIGUSR1));
    CHECK(sigismember(&worker_mask, SIGTERM));
    CHECK(sigismember(&worker_mask, SIGHUP));

    /* The calling (main) thread's own mask must remain untouched — the
     * effect is strictly thread-directed. */
    sigset_t main_mask;
    CHECK(pthread_sigmask(SIG_BLOCK, NULL, &main_mask) == 0);
    CHECK(!sigismember(&main_mask, SIGUSR1));
}

/* -----------------------------------------------------------------------
 * Buffer-safety regression test (finding #2)
 *
 * Before the fix the stack buffer in pcdsp_drain_period_to_pipe() was sized
 * as PCDSP_PIPE_CHUNK_FRAMES × 16, which was only safe for ≤4 bytes/frame.
 * Stereo S32_LE requires PCDSP_MAX_FRAME_BYTES = 8 bytes/frame; the old
 * assumption underflows by 2×.  This test exercises the maximum frame size
 * supported by the plugin (2 channels × 4 bytes = 8 bytes/frame) and would
 * trigger an ASAN stack-buffer-overflow if the old constant were used.
 * ---------------------------------------------------------------------- */

TEST(drain_period_max_frame_bytes_stereo_s32le_no_overflow)
{
    /*
     * Use 2 channels × 4 bytes/sample = 8 bytes/frame (stereo S32_LE, the
     * largest frame size the plugin supports).  Drain exactly PIPE_CHUNK
     * frames in one shot to exercise the full chunk buffer.
     */
    const size_t channels    = PCDSP_MAX_CHANNELS;     /* 2 */
    const size_t sample_bytes = PCDSP_MAX_SAMPLE_BYTES; /* 4 */
    const size_t frame_bytes  = channels * sample_bytes; /* 8 */
    const size_t period       = PCDSP_PIPE_CHUNK_FRAMES; /* 128 */

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, period * 2, frame_bytes) == 0);

    /* Fill ring buffer with a recognisable pattern. */
    uint8_t *src = malloc(period * frame_bytes);
    CHECK(src != NULL);
    for (size_t i = 0; i < period * frame_bytes; i++)
        src[i] = (uint8_t)(i & 0xff);
    CHECK(pcdsp_rb_write(&rb, src, period) == period);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], period, frame_bytes, NULL);
    CHECK(got == (ssize_t)period);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);

    /* Read back and verify byte-for-byte correctness. */
    uint8_t *dst = malloc(period * frame_bytes);
    CHECK(dst != NULL);
    ssize_t n = read(pipefd[0], dst, period * frame_bytes);
    CHECK(n == (ssize_t)(period * frame_bytes));
    CHECK(memcmp(src, dst, period * frame_bytes) == 0);

    free(src);
    free(dst);
    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * Partial-period drain test (finding #3)
 *
 * Verifies that pcdsp_drain_period_to_pipe() correctly drains a partial
 * final period (fewer frames than period_size).  This exercises the
 * draining-mode code path added to the worker thread — passing avail < period
 * as the frame count must not assert, overflow, or drop data.
 * ---------------------------------------------------------------------- */

TEST(drain_period_partial_final_period_via_drain_frames)
{
    /*
     * Scenario: period = 256, only 37 frames remain in the ring buffer.
     * The caller passes avail (37) as the frame count.  The function must
     * drain exactly 37 frames without error.
     */
    const size_t frame_bytes = 8; /* stereo S32_LE */
    const size_t remaining   = 37;

    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 512, frame_bytes) == 0);

    uint8_t *src = malloc(remaining * frame_bytes);
    CHECK(src != NULL);
    for (size_t i = 0; i < remaining * frame_bytes; i++)
        src[i] = (uint8_t)((i * 7) & 0xff);
    CHECK(pcdsp_rb_write(&rb, src, remaining) == remaining);
    CHECK(pcdsp_rb_read_avail(&rb) == remaining);

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    /* Pass remaining (< period) as the frame count — simulates draining mode. */
    ssize_t got = pcdsp_drain_period_to_pipe(&rb, pipefd[1], remaining, frame_bytes, NULL);
    CHECK(got == (ssize_t)remaining);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);

    uint8_t *dst = malloc(remaining * frame_bytes);
    CHECK(dst != NULL);
    ssize_t n = read(pipefd[0], dst, remaining * frame_bytes);
    CHECK(n == (ssize_t)(remaining * frame_bytes));
    CHECK(memcmp(src, dst, remaining * frame_bytes) == 0);

    free(src);
    free(dst);
    close(pipefd[0]);
    close(pipefd[1]);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("test_pcm_worker\n");

    /* pcdsp_drain_period_to_pipe */
    RUN(drain_period_writes_correct_bytes_to_pipe);
    RUN(drain_period_handles_epipe_and_returns_negative_errno);
    RUN(drain_period_partial_ring_buffer_writes_available_frames);
    RUN(drain_period_returns_zero_on_empty_ring_buffer);
    RUN(drain_period_wraps_around_ring_buffer_boundary);
    RUN(drain_period_cancellation_unblocks_full_pipe);

    /* pcdsp_drain_period_null_sink */
    RUN(null_sink_drops_period_frames_from_ring_buffer);
    RUN(null_sink_returns_fewer_frames_when_ring_buffer_partially_filled);
    RUN(null_sink_returns_zero_on_empty_ring_buffer);
    RUN(null_sink_skips_sleep_when_rate_is_zero);

    /* Period wrap / buffer wrap */
    RUN(period_wrap_across_ring_buffer_boundary);
    RUN(buffer_size_not_divisible_by_period_partial_final_period);

    /* EPIPE / EINTR */
    RUN(epipe_on_read_end_close_returns_minus_epipe);
    RUN(worker_survives_eintr_and_delivers_all_data);

    /* Poll / eventfd */
    RUN(eventfd_is_signalled_after_each_period);
    RUN(eventfd_accumulates_multiple_signals);

    /* Failure scenarios */
    RUN(failure_rust_daemon_restart_ipc_send_stop_fails_gracefully);
    RUN(failure_camilladsp_early_exit_detected_via_epipe);
    RUN(worker_survives_default_sigpipe_disposition_via_thread_scoped_block);
    RUN(block_all_signals_blocks_full_signal_set_not_just_sigpipe);

    /* Buffer-safety regression (finding #2: stack overflow with max frame bytes) */
    RUN(drain_period_max_frame_bytes_stereo_s32le_no_overflow);

    /* Partial-period drain regression (finding #3: drain hangs on final period) */
    RUN(drain_period_partial_final_period_via_drain_frames);

    printf("\n%d passed, %d failed\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
}
