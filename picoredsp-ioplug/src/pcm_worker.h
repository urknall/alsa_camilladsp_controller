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

#include <pthread.h>
#include <stddef.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <sys/types.h>
#include <time.h>

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
 * pcdsp_worker_block_all_signals — block all signals for the calling thread
 * only (thread-directed via pthread_sigmask, never process-wide).
 *
 * Must be called once, at the start of the dedicated pipe-writer thread
 * (production: the ioplug worker thread; see worker_thread() in pcm.c).
 * Matches BlueALSA's `io_thread_setup()` signal-masking pattern in
 * `bluealsa-pcm.c` (`sigfillset()` + `pthread_sigmask(SIG_SETMASK, ...)`)
 * rather than blocking only SIGPIPE, for the same two reasons documented
 * there:
 *
 *   (a) write() to a pipe whose read end has been closed (CamillaDSP
 *       exited) raises SIGPIPE; with the default disposition this
 *       terminates the *entire* process — unacceptable here because this
 *       plugin is loaded inside an arbitrary host application (Squeezelite,
 *       AirPlay receivers, etc.), not a process we control. Blocking (not
 *       ignoring) SIGPIPE keeps write() returning -EPIPE, which
 *       pcdsp_drain_period_to_pipe() already handles, without changing
 *       process-wide signal disposition via signal(SIGPIPE, SIG_IGN).
 *   (b) blocking the *full* signal set (not just SIGPIPE) for this thread
 *       guarantees no other signal can ever interrupt it asynchronously —
 *       this worker has no signal-driven control path today, but a stray
 *       process-wide signal (delivered to an arbitrary thread by the
 *       kernel) hitting this thread mid-write would otherwise be an
 *       unnecessary source of EINTR/undefined interaction with a host
 *       application's own signal handlers. Blocking everything up front
 *       removes that class of surprise entirely, matching BlueALSA's
 *       stated rationale in its IO thread setup.
 *
 * pthread_sigmask(SIG_SETMASK, &fullset, NULL) only changes the calling
 * thread's signal mask (thread-directed, per POSIX), so this confines the
 * fix to the dedicated pipe-writer thread instead of calling
 * signal(SIGPIPE, SIG_IGN) or sigprocmask() (which would affect disposition
 * or the whole process). The mask is intentionally never restored: a
 * dedicated pipe-writer thread has no other purpose for its entire
 * lifetime, so there is no later point at which re-enabling delivery of a
 * signal that may have gone pending while blocked would be safe.
 */
void pcdsp_worker_block_all_signals(void);

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

/*
 * pcdsp_wait_pause_ack — block until the worker acknowledges a pause
 * request, or until it stops running, or until `deadline` passes.
 *
 * The caller must hold `mutex` locked on entry; it is left locked on
 * return (both on success and on timeout) so the caller can continue
 * inspecting/mutating the shared pause state before unlocking.
 *
 * @mutex     Lock guarding `*ack` and `pause_cond`'s predicate. Must
 *            already be held by the caller.
 * @cond      Condition variable the worker broadcasts on after setting
 *            `*ack` (see worker_thread() in pcm.c).
 * @ack       Set to true by the worker once it has reached a safe point
 *            (parked, not mid-write to the pipe).
 * @running   Worker-running flag. If it becomes false while waiting, the
 *            worker cannot be mid-write (it has exited), so the "no write
 *            in flight" invariant already holds without an explicit ack.
 * @deadline  Absolute CLOCK_REALTIME deadline for pthread_cond_timedwait().
 *
 * Returns 0 if the invariant "no write is in flight" is confirmed (either
 * `*ack` became true, or the worker stopped running), or -ETIMEDOUT if
 * `deadline` was reached while the worker was still running and had not
 * acknowledged the pause — i.e. the invariant could NOT be confirmed and
 * the caller must not treat the pause as having taken effect.
 */
int pcdsp_wait_pause_ack(pthread_mutex_t      *mutex,
                          pthread_cond_t       *cond,
                          const bool           *ack,
                          const _Atomic(bool)  *running,
                          const struct timespec *deadline);

#ifdef __cplusplus
}
#endif

#endif /* PICOREDSP_PCM_WORKER_H */
