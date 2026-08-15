/*
 * picoredsp-ioplug — worker pipe-drain helpers
 *
 * pcm_worker.c — implementation of the period-drain helpers declared in
 * pcm_worker.h.  These functions are extracted from the worker thread in
 * pcm.c so that they can be unit-tested independently of the ALSA ioplug
 * framework.
 */

#define _GNU_SOURCE

#include "pcm_worker.h"
#include "ringbuffer.h"

#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <time.h>
#include <unistd.h>

/* -----------------------------------------------------------------------
 * pcdsp_worker_block_all_signals
 * ---------------------------------------------------------------------- */

int pcdsp_worker_block_all_signals(void)
{
    sigset_t set;
    sigfillset(&set);
    /* Thread-directed: SIG_SETMASK replaces (not merely adds to) the
     * calling thread's signal mask, affecting only this thread — matching
     * BlueALSA's io_thread_setup() exactly rather than only blocking
     * SIGPIPE. See the rationale in pcm_worker.h.
     *
     * Matching BlueALSA: check the return value rather than assuming
     * success. pthread_sigmask() returns a positive errno (not -1/errno)
     * on failure; the SIGPIPE-safety invariant this establishes is load
     * bearing (a broken pipe must report -EPIPE from write(), not kill the
     * host process), so a caller that ignored a failure here would be
     * silently assuming a safety property that was never actually
     * established. */
    int rc = pthread_sigmask(SIG_SETMASK, &set, NULL);
    if (rc != 0)
        return -rc;
    return 0;
}

/* -----------------------------------------------------------------------
 * pcdsp_drain_period_to_pipe
 * ---------------------------------------------------------------------- */

ssize_t pcdsp_drain_period_to_pipe(pcdsp_ringbuffer_t  *rb,
                                    int                  pipe_fd,
                                    size_t               period_frames,
                                    size_t               frame_bytes,
                                    const _Atomic(bool) *keep_running)
{
    /* Temporary stack buffer — avoids heap allocation in the hot path.
     * Sized for the worst case: PCDSP_PIPE_CHUNK_FRAMES × PCDSP_MAX_FRAME_BYTES.
     * PCDSP_MAX_FRAME_BYTES = PCDSP_MAX_CHANNELS(2) × PCDSP_MAX_SAMPLE_BYTES(4) = 8.
     * Any increase to the channel limit or sample width must update PCDSP_MAX_FRAME_BYTES
     * in pcm_worker.h — this buffer is intentionally derived from those constants. */
    uint8_t tmp[PCDSP_PIPE_CHUNK_FRAMES * PCDSP_MAX_FRAME_BYTES];

    size_t frames_written = 0;
    size_t frames_left    = period_frames;

    while (frames_left > 0) {
        size_t chunk = frames_left < PCDSP_PIPE_CHUNK_FRAMES
                       ? frames_left : PCDSP_PIPE_CHUNK_FRAMES;

        /* Peek (copy without removing) rather than pcdsp_rb_read(): if this
         * chunk were removed from the ring buffer now, pcdsp_drain()'s
         * completion check (pcdsp_rb_read_avail(&rb) == 0) could observe an
         * empty ring buffer — and therefore return success — while these
         * exact frames are still sitting in `tmp[]` below, not yet handed to
         * the pipe (or stuck retrying against a stalled/full pipe). The ring
         * buffer must remain the single authoritative queue for "has this
         * audio left the plugin yet?" until the write below actually
         * succeeds; only then does pcdsp_rb_drop() commit the removal. */
        size_t got = pcdsp_rb_peek(rb, tmp, chunk);
        if (got == 0)
            break; /* ring buffer drained before period was complete */

        size_t  byte_count = got * frame_bytes;
        ssize_t byte_written = 0;

        while ((size_t)byte_written < byte_count) {
            if (keep_running &&
                !atomic_load_explicit(keep_running, memory_order_acquire))
                return (ssize_t)frames_written;

            /* The production pipe fd is O_NONBLOCK.  Poll in short bounded
             * intervals so a stop request can terminate a worker even when
             * CamillaDSP is alive but no longer reading stdin. */
            struct pollfd pfd = { .fd = pipe_fd, .events = POLLOUT };
            int pr = poll(&pfd, 1, 20);
            if (pr < 0) {
                if (errno == EINTR)
                    continue;
                return -errno;
            }
            if (pr == 0)
                continue;
            if (pfd.revents & POLLNVAL)
                return -EBADF;
            if (pfd.revents & (POLLERR | POLLHUP))
                return -EPIPE;
            if (!(pfd.revents & POLLOUT))
                continue;

            ssize_t n = write(pipe_fd,
                              tmp + (size_t)byte_written,
                              byte_count - (size_t)byte_written);
            if (n < 0) {
                if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)
                    continue;
                /* EPIPE or other hard error — CamillaDSP has gone */
                return -errno;
            }
            if (n == 0)
                continue;
            byte_written += n;
        }

        /* The whole chunk has now actually reached the pipe — only now is
         * it safe to remove it from the ring buffer. If the loop above had
         * returned early (cancellation or error), these frames are still in
         * the ring buffer, exactly as if they had never been touched. */
        pcdsp_rb_drop(rb, got);

        frames_written += got;
        frames_left    -= got;
    }

    return (ssize_t)frames_written;
}

/* -----------------------------------------------------------------------
 * pcdsp_drain_period_null_sink
 * ---------------------------------------------------------------------- */

size_t pcdsp_drain_period_null_sink(pcdsp_ringbuffer_t *rb,
                                     size_t              period_frames,
                                     unsigned long       rate)
{
    size_t drained = pcdsp_rb_drop(rb, period_frames);
    if (drained == 0)
        return 0;

    if (rate > 0) {
        /* Sleep for the nominal period duration to pace the null sink. */
        unsigned long period_ns =
            1000000000UL * (unsigned long)period_frames / rate;
        struct timespec ts = {
            .tv_sec  = (time_t)(period_ns / 1000000000UL),
            .tv_nsec = (long)(period_ns % 1000000000UL),
        };
        nanosleep(&ts, NULL);
    }

    return drained;
}

/* -----------------------------------------------------------------------
 * pcdsp_wait_pause_ack
 * ---------------------------------------------------------------------- */

int pcdsp_wait_pause_ack(pthread_mutex_t      *mutex,
                          pthread_cond_t       *cond,
                          const bool           *ack,
                          const _Atomic(bool)  *running,
                          const struct timespec *deadline)
{
    while (!(*ack) &&
           atomic_load_explicit(running, memory_order_acquire)) {
        int wr = pthread_cond_timedwait(cond, mutex, deadline);
        if (wr == ETIMEDOUT)
            break;
    }

    if (!(*ack) && atomic_load_explicit(running, memory_order_acquire))
        return -ETIMEDOUT;

    return 0;
}
