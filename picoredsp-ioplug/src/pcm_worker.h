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
 * pcdsp_worker_block_sigpipe — block SIGPIPE for the calling thread only.
 *
 * Must be called once, at the start of any thread that writes to the
 * CamillaDSP stdin pipe (production: the ioplug worker thread; see
 * worker_thread() in pcm.c).  write() to a pipe whose read end has been
 * closed (CamillaDSP exited) raises SIGPIPE; with the default disposition
 * this terminates the *entire* process — unacceptable here because this
 * plugin is loaded inside an arbitrary host application (Squeezelite,
 * AirPlay receivers, etc.), not a process we control.
 *
 * pthread_sigmask() only changes the calling thread's signal mask, so this
 * confines the fix to the dedicated pipe-writer thread instead of calling
 * signal(SIGPIPE, SIG_IGN), which would change disposition for the entire
 * process — including threads owned by the host application.
 *
 * With SIGPIPE blocked (not ignored) for this thread, write() still returns
 * -EPIPE on a broken pipe (the syscall failure is independent of whether the
 * generated signal is delivered), which pcdsp_drain_period_to_pipe() already
 * handles.  The mask is intentionally never restored: a dedicated pipe-writer
 * thread has no other purpose for its entire lifetime, so there is no later
 * point at which re-enabling delivery of a signal that may have gone pending
 * while blocked would be safe.
 */
void pcdsp_worker_block_sigpipe(void);

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
