/*
 * picoredsp-ioplug — worker pipe-drain helpers
 *
 * pcm_worker.h — functions extracted from the worker thread so that the
 * pipe-drain and null-sink logic can be unit-tested without the ALSA
 * ioplug framework.
 *
 * These functions are called by the worker thread in pcm.c; they are also
 * linked into the test_pcm_worker test binary via pcdsp_internals.
 */

#ifndef PICOREDSP_PCM_WORKER_H
#define PICOREDSP_PCM_WORKER_H

#include "ringbuffer.h"

#include <stddef.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Chunk size used by pcdsp_drain_period_to_pipe (stack buffer, not heap). */
#define PCDSP_PIPE_CHUNK_FRAMES   128u

/* Maximum supported channels (stereo constraint).
 * All plugin trust boundaries enforce channels == PCDSP_MAX_CHANNELS. */
#define PCDSP_MAX_CHANNELS        2u

/* Maximum physical bytes per sample for any supported format (S32_LE / FLOAT_LE). */
#define PCDSP_MAX_SAMPLE_BYTES    4u

/* Maximum frame size in bytes = PCDSP_MAX_CHANNELS × PCDSP_MAX_SAMPLE_BYTES.
 * The stack buffer in pcdsp_drain_period_to_pipe is sized on this constant, so
 * adding a new format or increasing the channel limit here is the single change
 * needed to keep the stack buffer safe. */
#define PCDSP_MAX_FRAME_BYTES     (PCDSP_MAX_CHANNELS * PCDSP_MAX_SAMPLE_BYTES)

/*
 * pcdsp_drain_period_to_pipe — drain one period worth of frames from `rb`
 * and write them into `pipe_fd`.
 *
 * @rb            Ring buffer to drain from.
 * @pipe_fd       Write end of the CamillaDSP stdin pipe (must be >= 0).
 * @period_frames Number of frames in one period.
 * @frame_bytes   Bytes per frame (format × channels).
 *
 * @keep_running  Optional atomic run flag.  When non-NULL, pipe waits are
 *                bounded and return promptly once the flag becomes false.
 *                Pass NULL for callers that do not need cancellation.
 *
 * Returns the number of frames successfully written (0 .. period_frames) on
 * success, or a negative errno on pipe error (e.g. -EPIPE when CamillaDSP
 * has exited).  EINTR and EAGAIN are retried internally.
 *
 * The production caller uses a non-blocking pipe fd plus `keep_running`; this
 * prevents stream shutdown from depending on close(2) interrupting a write in
 * another thread (which Linux does not guarantee).
 */
ssize_t pcdsp_drain_period_to_pipe(pcdsp_ringbuffer_t  *rb,
                                    int                  pipe_fd,
                                    size_t               period_frames,
                                    size_t               frame_bytes,
                                    const _Atomic(bool) *keep_running);

/*
 * pcdsp_drain_period_null_sink — drain one period worth of frames from `rb`
 * and sleep for the nominal period duration (rate-paced null sink).
 *
 * Used as a fallback when no pipe fd is available (controller absent, unit
 * tests).
 *
 * @rb            Ring buffer to drain from.
 * @period_frames Number of frames in one period.
 * @rate          Sample rate in Hz (used to compute the sleep duration).
 *                Pass 0 to skip the sleep (useful in tests that need to run
 *                without a real-time constraint).
 *
 * Returns the number of frames actually dropped (may be less than
 * period_frames if the ring buffer was not full).
 */
size_t pcdsp_drain_period_null_sink(pcdsp_ringbuffer_t *rb,
                                     size_t              period_frames,
                                     unsigned long       rate);

#ifdef __cplusplus
}
#endif

#endif /* PICOREDSP_PCM_WORKER_H */
