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
#include <stdint.h>
#include <time.h>
#include <unistd.h>

/* -----------------------------------------------------------------------
 * pcdsp_drain_period_to_pipe
 * ---------------------------------------------------------------------- */

ssize_t pcdsp_drain_period_to_pipe(pcdsp_ringbuffer_t *rb,
                                    int                 pipe_fd,
                                    size_t              period_frames,
                                    size_t              frame_bytes)
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
        size_t got = pcdsp_rb_read(rb, tmp, chunk);
        if (got == 0)
            break; /* ring buffer drained before period was complete */

        size_t  byte_count = got * frame_bytes;
        ssize_t byte_written = 0;

        while ((size_t)byte_written < byte_count) {
            ssize_t n = write(pipe_fd,
                              tmp + (size_t)byte_written,
                              byte_count - (size_t)byte_written);
            if (n < 0) {
                if (errno == EINTR)
                    continue; /* retry on signal */
                /* EPIPE or other hard error — CamillaDSP has gone */
                return -errno;
            }
            byte_written += n;
        }

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
