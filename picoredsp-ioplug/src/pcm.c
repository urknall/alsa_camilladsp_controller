/*
 * picoredsp-ioplug — ALSA ioplug PCM plugin (Gate 8 stdin pipe transport)
 *
 * Architecture overview
 * ---------------------
 * This file implements the ALSA ioplug callback table.
 *
 * Gate 7 added the START / READY handshake:
 *   ✓ hw_params connects to the Rust controller socket
 *   ✓ plugin sends HELLO (version negotiation)
 *   ✓ plugin sends START(rate, format, channels)
 *   ✓ plugin waits for READY (or ERROR) before allowing PCM transfer
 *   ✓ stop closes the local data path first, then sends STOP to the
 *     controller (see pcdsp_stop() for why this order matters)
 *
 * Gate 8 adds the stdin pipe transport:
 *   ✓ Rust controller creates a pipe, spawns CamillaDSP with read-end as stdin
 *   ✓ Rust sends READY with the pipe write-end via SCM_RIGHTS
 *   ✓ plugin receives the write-end fd in pcdsp_ipc_recv_ready()
 *   ✓ worker thread drains the ring buffer by writing to the pipe fd
 *   ✓ on stop, the plugin stops the worker and closes its fd *before*
 *     notifying the controller; Rust also closes its copy
 *   ✓ CamillaDSP sees EOF on stdin and shuts down
 *
 * Data path (Gate 8):
 *   Application → ALSA mmap area → pcdsp_transfer() → ring buffer
 *   worker thread → reads ring buffer → writes to pipe_fd
 *   pipe_fd → kernel pipe → CamillaDSP stdin → DSP → DAC
 *
 * Rust is never in the PCM data path.
 *
 * Thread safety
 * -------------
 * ALSA calls start/stop/transfer/pointer/pause inside its own mutex.
 * The worker thread must not call back into the ioplug API; it only
 * reads from the ring buffer and writes to the pipe fd.
 *
 * Eventfd-based poll
 * ------------------
 * We use a single eventfd as the poll descriptor.  The worker signals
 * it (writes 1) each time it completes a period worth of consumption,
 * indicating to the application that more space is available.
 *
 * Drain and pause synchronisation
 * --------------------------------
 * pcdsp_drain() waits for the ring buffer to empty, bounded by a timeout
 * computed from the periods remaining to drain (pcdsp_drain_timeout_ns(),
 * matching BlueALSA's `100ms + periods_remaining * period_time` formula) so
 * a CamillaDSP that stops reading stdin without signalling EPIPE cannot
 * block snd_pcm_drain() forever.
 * pcdsp_pause(enable=1) blocks (bounded by PCDSP_PAUSE_ACK_TIMEOUT_NS) until
 * the worker acknowledges via pause_mutex/pause_cond that it has reached a
 * safe point (parked, not mid-write) — so pause() cannot return while a
 * write to the pipe is still in flight.
 *
 * sw_params / avail_min and delay() accounting
 * ---------------------------------------------
 * pcdsp_sw_params() records the application's negotiated avail_min so
 * pcdsp_poll_revents() only reports readiness once that many frames are
 * free, instead of waking on any single available frame. pcdsp_delay()
 * additionally counts frames already handed off to the kernel pipe (via
 * FIONREAD) on top of the ring buffer, since those frames have left the
 * ring buffer but have not yet reached CamillaDSP/the DAC. A small,
 * documented gap remains: (a) the worker pulls frames out of the ring
 * buffer in PCDSP_PIPE_CHUNK_FRAMES-sized chunks before writing them to
 * the pipe, so at most one in-flight chunk can transiently be counted by
 * neither the ring buffer nor FIONREAD; (b) CamillaDSP's own internal
 * buffering (resampler/filter/pipeline latency) once bytes leave the pipe
 * is not visible to the plugin and is not accounted for here.
 */

#define _GNU_SOURCE

#include <alsa/asoundlib.h>
#include <alsa/pcm_external.h>
#include <alsa/pcm_ioplug.h>

#include "format.h"
#include "ipc.h"
#include "pcm_worker.h"
#include "ringbuffer.h"
#include "timing.h"

#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

/* -----------------------------------------------------------------------
 * Constants
 * ---------------------------------------------------------------------- */

/* Ring buffer size expressed as a multiple of the maximum period size.
 * Must be a power of two. */
#define RB_PERIODS  8u

/* Supported sample rates advertised to ALSA. */
static const unsigned int k_rates[] = {
    44100, 48000, 88200, 96000, 176400, 192000
};

/* Minimum and maximum period / buffer sizes in frames. */
#define PERIOD_SIZE_MIN   64u
#define PERIOD_SIZE_MAX   8192u
#define BUFFER_SIZE_MIN   (PERIOD_SIZE_MIN * 2u)
#define BUFFER_SIZE_MAX   (PERIOD_SIZE_MAX * RB_PERIODS)

/* pcdsp_drain() fallback ceiling, used only if rate or period_size is
 * unexpectedly zero at drain() time (should not happen post-hw_params).
 * The primary bound is computed dynamically by pcdsp_drain_timeout_ns()
 * from the periods actually remaining to drain, matching BlueALSA's
 * `100ms + periods_remaining * period_time` formula instead of a flat
 * constant — see that function for the rationale. */
#define PCDSP_DRAIN_TIMEOUT_NS   (5ULL * 1000000000ULL) /* 5 s */

/* pcdsp_pause(enable=1) ceiling: bounds how long pause() waits for the
 * worker thread to acknowledge it has reached a safe point (i.e. is not
 * mid-write to the pipe) before returning anyway. Guards against wedging
 * pause() forever if the worker has stopped responding for an unrelated
 * reason. */
#define PCDSP_PAUSE_ACK_TIMEOUT_NS  (2ULL * 1000000000ULL) /* 2 s */

/* -----------------------------------------------------------------------
 * Plugin private data
 * ---------------------------------------------------------------------- */

typedef struct pcdsp_pcm {
    snd_pcm_ioplug_t    io;          /* MUST be first — cast back via container_of */

    /* hw_params-negotiated values */
    size_t              frame_bytes;
    snd_pcm_uframes_t   period_size;
    snd_pcm_uframes_t   buffer_size;

    /* sw_params-negotiated avail_min (frames). Gates poll_revents() readiness
     * so the application is only woken once at least this many frames are
     * free, matching real ALSA driver semantics. Defaults to 1 (wake as soon
     * as any space is free) until/unless ALSA calls pcdsp_sw_params(). */
    snd_pcm_uframes_t   avail_min;

    /* Ring buffer */
    pcdsp_ringbuffer_t  rb;

    /* Monotonic stream timer (null-sink drain pacing) */
    pcdsp_stream_timer_t timer;

    /* hw_ptr as tracked by the worker */
    _Atomic(uint64_t)   hw_frames;   /* total frames consumed (monotone) */

    /* Eventfd for poll */
    int                 event_fd;

    /* Worker thread */
    pthread_t           worker;
    _Atomic(bool)       worker_running;
    bool                worker_joinable;
    _Atomic(bool)       paused;
    /* Set to true by pcdsp_drain() while a drain is in progress so that the
     * worker consumes partial periods rather than waiting for a full one.
     * Cleared by pcdsp_drain() after the ring buffer is empty. */
    _Atomic(bool)       draining;
    /* Non-zero when the worker has detected a fatal stream error (e.g. -EPIPE
     * when CamillaDSP exits).  Checked by pcdsp_pointer() and
     * pcdsp_poll_revents() so applications are woken and see the error. */
    _Atomic(int)        stream_error;

    /* Pause acknowledgement.  Guarded by pause_mutex/pause_cond (not a plain
     * atomic) so pcdsp_pause(enable=1) can *block* until the worker reaches a
     * safe point — i.e. it has observed `paused` and is parked, not mid-write
     * to the pipe — instead of returning to ALSA immediately and racing the
     * worker's in-flight write. */
    pthread_mutex_t     pause_mutex;
    pthread_cond_t      pause_cond;
    bool                worker_paused_ack;

    /* IPC connection to the Rust controller */
    pcdsp_ipc_conn_t    conn;
    /* Write end of the stdin pipe received from the Rust controller via
     * SCM_RIGHTS in the READY message.  -1 when no pipe is active. */
    int                 pipe_fd;
    /* AF_UNIX socket path (from ALSA config or default) */
    char                socket_path[108]; /* UNIX_PATH_MAX */
} pcdsp_pcm_t;

static void pcdsp_signal_event_fd(int event_fd)
{
    uint64_t val = 1;
    ssize_t  rc  = write(event_fd, &val, sizeof(val));
    if (rc < 0 && errno != EAGAIN && errno != EINTR)
        SNDERR("picoredsp: failed to signal eventfd: %s", strerror(errno));
}

/* Stop and reap the worker without mutating pipe_fd while the worker may be
 * using it.  `worker_joinable` is separate from `worker_running` because the
 * worker can clear the latter itself after EPIPE but still needs pthread_join. */
static void pcdsp_stop_worker(pcdsp_pcm_t *pcdsp)
{
    atomic_store_explicit(&pcdsp->worker_running, false, memory_order_release);
    atomic_store_explicit(&pcdsp->paused, false, memory_order_release);

    /* Wake a worker parked in the pause wait (pcdsp_pause condvar) so it
     * re-checks worker_running and exits instead of waiting for the ack
     * timeout. */
    pthread_mutex_lock(&pcdsp->pause_mutex);
    pthread_cond_broadcast(&pcdsp->pause_cond);
    pthread_mutex_unlock(&pcdsp->pause_mutex);

    if (pcdsp->worker_joinable) {
        pcdsp_signal_event_fd(pcdsp->event_fd);
        pthread_join(pcdsp->worker, NULL);
        pcdsp->worker_joinable = false;
    }
}

#define io_to_pcdsp(io_ptr) ((pcdsp_pcm_t *)(io_ptr))

/* -----------------------------------------------------------------------
 * Worker thread — pipe writer (Gate 8) with null-sink fallback
 *
 * When `pipe_fd >= 0` (Gate 8): reads one period's worth of frames from the
 * ring buffer and writes them directly into the CamillaDSP stdin pipe.
 * No rate-pacing sleep is needed.  The pipe fd is non-blocking; the helper
 * waits for POLLOUT in short intervals so shutdown remains cancellable while
 * CamillaDSP's read rate still provides backpressure.
 *
 * When `pipe_fd < 0` (fallback): discards frames at the nominal sample rate
 * using nanosleep (original null-sink behaviour, preserved for unit tests and
 * the case where no controller is connected).
 *
 * After each period the eventfd is signalled so the application's poll()
 * returns writable.
 * ---------------------------------------------------------------------- */

static void *worker_thread(void *arg)
{
    pcdsp_pcm_t *pcdsp = arg;

    /* This thread writes to the CamillaDSP stdin pipe; block *all* signals
     * for it (thread-scoped, not process-wide) — matching BlueALSA's
     * io_thread_setup() exactly, not just SIGPIPE — so a broken pipe
     * reports -EPIPE via write() instead of killing the host application,
     * and no other signal can interrupt this thread mid-transfer. See
     * pcdsp_worker_block_all_signals() in pcm_worker.c for the full
     * rationale. */
    pcdsp_worker_block_all_signals();

    while (atomic_load_explicit(&pcdsp->worker_running, memory_order_acquire)) {
        if (atomic_load_explicit(&pcdsp->paused, memory_order_acquire)) {
            /* Reached a safe point: no write is in flight. Acknowledge the
             * pause so a blocked pcdsp_pause(enable=1) can return, then wait
             * (bounded, so worker_running transitions are still observed
             * promptly) until resumed or told to stop. */
            pthread_mutex_lock(&pcdsp->pause_mutex);
            pcdsp->worker_paused_ack = true;
            pthread_cond_broadcast(&pcdsp->pause_cond);
            while (atomic_load_explicit(&pcdsp->paused, memory_order_acquire) &&
                   atomic_load_explicit(&pcdsp->worker_running, memory_order_acquire)) {
                struct timespec deadline;
                clock_gettime(CLOCK_REALTIME, &deadline);
                deadline.tv_nsec += 20000000L; /* 20 ms */
                if (deadline.tv_nsec >= 1000000000L) {
                    deadline.tv_nsec -= 1000000000L;
                    deadline.tv_sec  += 1;
                }
                pthread_cond_timedwait(&pcdsp->pause_cond, &pcdsp->pause_mutex,
                                        &deadline);
            }
            pcdsp->worker_paused_ack = false;
            pthread_mutex_unlock(&pcdsp->pause_mutex);
            continue;
        }

        bool is_draining = atomic_load_explicit(&pcdsp->draining, memory_order_acquire);
        size_t avail = pcdsp_rb_read_avail(&pcdsp->rb);

        if (avail < pcdsp->period_size) {
            if (!is_draining) {
                /* Normal operation: wait until a full period is buffered.
                 * Sleep for half a period to avoid busy-wait.
                 * Multiply before dividing to avoid integer truncation. */
                unsigned long rate     = pcdsp->io.rate ? pcdsp->io.rate : 48000;
                unsigned long sleep_ns = 500000000UL * (unsigned long)pcdsp->period_size / rate;
                struct timespec ts = { .tv_sec  = (time_t)(sleep_ns / 1000000000UL),
                                       .tv_nsec = (long)(sleep_ns % 1000000000UL) };
                nanosleep(&ts, NULL);
                continue;
            }
            /* Draining: consume whatever is available (even a partial period). */
            if (avail == 0) {
                /* Ring buffer is empty — yield briefly and re-check. */
                struct timespec ts = { .tv_nsec = 1000000 }; /* 1 ms */
                nanosleep(&ts, NULL);
                continue;
            }
        }

        /* Drain either a full period (normal) or the remaining frames (draining). */
        size_t drain_frames = (avail < pcdsp->period_size) ? avail : pcdsp->period_size;

        int pipe_fd = pcdsp->pipe_fd;
        if (pipe_fd >= 0) {
            /*
             * Gate 8: drain frames from the ring buffer and write them
             * directly into the CamillaDSP stdin pipe.
             *
             * pcdsp_drain_period_to_pipe() handles chunking, EINTR retry,
             * and returns the number of frames written or -errno on pipe
             * error (e.g. -EPIPE when CamillaDSP has exited).
             */
            ssize_t got = pcdsp_drain_period_to_pipe(
                &pcdsp->rb, pipe_fd, drain_frames, pcdsp->frame_bytes,
                &pcdsp->worker_running);
            if (got < 0) {
                /* EPIPE or other hard error: CamillaDSP has gone.
                 * Record the error so pcdsp_pointer() and pcdsp_poll_revents()
                 * can expose it to the application, then wake the poll fd so
                 * the application is not left sleeping in poll(). */
                atomic_store_explicit(&pcdsp->stream_error, (int)got,
                                      memory_order_release);
                atomic_store_explicit(&pcdsp->worker_running,
                                      false, memory_order_release);
                pcdsp_signal_event_fd(pcdsp->event_fd);
                goto done;
            }
            if (got > 0)
                atomic_fetch_add_explicit(&pcdsp->hw_frames, (uint64_t)got,
                                          memory_order_release);
        } else {
            /* Fallback (no pipe): null-sink drain with nominal-rate pacing. */
            unsigned long rate2  = pcdsp->io.rate;
            size_t drained = pcdsp_drain_period_null_sink(
                &pcdsp->rb, drain_frames, rate2);
            if (drained == 0)
                continue;
            atomic_fetch_add_explicit(&pcdsp->hw_frames, (uint64_t)drained,
                                      memory_order_release);
        }

        /* Signal poll fd — one period of space is newly available. */
        uint64_t val = 1;
        if (write(pcdsp->event_fd, &val, sizeof(val)) < 0 && errno != EAGAIN) {
            /* Non-fatal; the application will catch up via pointer(). */
        }
    }
done:
    return NULL;
}

/* -----------------------------------------------------------------------
 * ioplug callbacks
 * ---------------------------------------------------------------------- */

static int pcdsp_start(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    atomic_store_explicit(&pcdsp->paused, false, memory_order_release);
    pcdsp_timer_start(&pcdsp->timer);

    /* Start worker if not already running. */
    if (!atomic_load_explicit(&pcdsp->worker_running, memory_order_acquire)) {
        /* Reap a worker that exited on its own (for example after EPIPE). */
        if (pcdsp->worker_joinable) {
            pthread_join(pcdsp->worker, NULL);
            pcdsp->worker_joinable = false;
        }

        atomic_store_explicit(&pcdsp->worker_running, true, memory_order_release);
        int thread_rc = pthread_create(&pcdsp->worker, NULL, worker_thread, pcdsp);
        if (thread_rc != 0) {
            atomic_store_explicit(&pcdsp->worker_running, false, memory_order_release);
            return -thread_rc;
        }
        pcdsp->worker_joinable = true;
    }

    return 0;
}

static int pcdsp_stop(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    /* Shut down the local data path *before* notifying the controller:
     * stop/reap the worker thread, then close our pipe write-end so
     * CamillaDSP sees EOF once Rust also closes its copy.  The worker uses
     * bounded non-blocking pipe writes, so it observes worker_running=false
     * promptly.
     *
     * Ordering matters: sending STOP first (as this used to do) let Rust
     * proceed to supervisor.stop_stream() — which waits for CamillaDSP to
     * exit — while the worker thread here could still be mid-write. Tearing
     * down the producer/data path first guarantees that by the time the
     * controller learns the stream ended, nothing on the plugin side can
     * still be writing to CamillaDSP's stdin. */
    pcdsp_stop_worker(pcdsp);
    pcdsp_timer_stop(&pcdsp->timer);

    if (pcdsp->pipe_fd >= 0) {
        close(pcdsp->pipe_fd);
        pcdsp->pipe_fd = -1;
    }

    /* Gate 7: notify the controller that the stream is ending, now that the
     * local data path has already been fully torn down. */
    if (pcdsp->conn.fd >= 0)
        pcdsp_ipc_send_stop(&pcdsp->conn);

    /* Drain eventfd. */
    uint64_t dummy;
    while (read(pcdsp->event_fd, &dummy, sizeof(dummy)) > 0)
        ;

    return 0;
}

/*
 * pcdsp_disconnect_on_stream_error — check for a fatal stream error and, if
 * one is present, proactively transition the ioplug's visible ALSA state to
 * DISCONNECTED before returning it, matching BlueALSA's bluealsa_prepare(),
 * bluealsa_drain(), bluealsa_pause() and bluealsa_delay(), each of which
 * checks `!pcm->connected` at entry and calls
 * snd_pcm_ioplug_set_state(io, SND_PCM_STATE_DISCONNECTED) before returning
 * -ENODEV. Doing this from every callback (rather than only from pointer())
 * means the application observes SND_PCM_STATE_DISCONNECTED via
 * snd_pcm_state() as soon as it calls any of these, not only after its next
 * pointer()-driven avail update.
 *
 * Returns 0 if there is no recorded error, otherwise the recorded error
 * (always a negative errno).
 */
static int pcdsp_disconnect_on_stream_error(snd_pcm_ioplug_t *io, pcdsp_pcm_t *pcdsp)
{
    int serr = atomic_load_explicit(&pcdsp->stream_error, memory_order_acquire);
    if (serr != 0)
        snd_pcm_ioplug_set_state(io, SND_PCM_STATE_DISCONNECTED);
    return serr;
}

/*
 * pointer — return the current hw_ptr.
 *
 * The ioplug core uses this value to determine how much space is available
 * for the application to write.  We return a negative value to signal XRUN.
 *
 * When alsa-lib exposes SND_PCM_IOPLUG_FLAG_BOUNDARY_WA, return the monotone
 * hardware pointer so the ioplug core can distinguish "buffer empty" from
 * "buffer full" across wrap-around. Older alsa-lib versions lack that flag,
 * so fall back to modulo buffer_size there.
 */
static snd_pcm_sframes_t pcdsp_pointer(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    uint64_t hw_total = atomic_load_explicit(&pcdsp->hw_frames, memory_order_acquire);
    snd_pcm_sframes_t hw_ptr = (snd_pcm_sframes_t)hw_total;
#ifndef SND_PCM_IOPLUG_FLAG_BOUNDARY_WA
    hw_ptr %= (snd_pcm_sframes_t)pcdsp->buffer_size;
#endif

    /* If the worker recorded a fatal stream error (e.g. -EPIPE when
     * CamillaDSP exited), this PCM instance cannot recover on its own.
     * Matching BlueALSA's bluealsa_pointer(): alsa-lib's
     * snd_pcm_ioplug_hw_ptr_update() treats any negative pointer() return
     * as XRUN (or drops a draining stream), never as DISCONNECTED, which
     * would leave the fatal error looking recoverable to the application.
     * So instead of returning the negative errno here, set the ioplug
     * state to DISCONNECTED directly and return the last known
     * (non-negative) hw_ptr — ioplug then leaves that state alone, and
     * snd_pcm_avail_update()/poll_revents() surface -ENODEV to the caller
     * because the PCM is DISCONNECTED rather than merely in XRUN. */
    if (pcdsp_disconnect_on_stream_error(io, pcdsp) != 0)
        return hw_ptr;

    /* Check for XRUN: if the application has written more than buffer_size
     * frames ahead of what the consumer has drained, declare XRUN. */
    uint64_t rp = atomic_load_explicit(&pcdsp->rb.read_pos, memory_order_acquire);
    uint64_t wp = atomic_load_explicit(&pcdsp->rb.write_pos, memory_order_acquire);
    if ((wp - rp) > pcdsp->buffer_size)
        return -EPIPE;

    return hw_ptr;
}

/*
 * transfer — copy frames from the ALSA mmap area into the ring buffer.
 *
 * ALSA calls this inside its mutex after the application writes data.
 */
static snd_pcm_sframes_t pcdsp_transfer(snd_pcm_ioplug_t              *io,
                                        const snd_pcm_channel_area_t  *areas,
                                        snd_pcm_uframes_t              offset,
                                        snd_pcm_uframes_t              size)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    const uint8_t *src = (const uint8_t *)areas[0].addr
                         + (areas[0].first + areas[0].step * offset) / 8;

    size_t written = pcdsp_rb_write(&pcdsp->rb, src, (size_t)size);
    return (snd_pcm_sframes_t)written;
}

static int pcdsp_hw_params(snd_pcm_ioplug_t *io, snd_pcm_hw_params_t *params)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);
    (void)params;

    /* Enforce the stereo-only contract at the hw_params boundary.  The ALSA
     * constraint already limits negotiation to 2 channels, but validate here
     * as a defence-in-depth check. */
    if (io->channels != 2) {
        SNDERR("picoredsp: only stereo (2 channels) is supported, got %u",
               io->channels);
        return -EINVAL;
    }

    /* Compute frame size from negotiated format and channel count. */
    int rc = pcdsp_format_frame_bytes(io->format, io->channels, &pcdsp->frame_bytes);
    if (rc < 0)
        return rc;

    pcdsp->period_size = io->period_size;
    pcdsp->buffer_size = io->buffer_size;
    /* Reset to the wake-on-any-space default; pcdsp_sw_params() (called
     * after hw_params, if the application configures sw_params) will raise
     * this to the negotiated avail_min. */
    pcdsp->avail_min    = 1;

    /* Defensive renegotiation path: no worker may reference the old ring
     * buffer or pipe while hw_params replaces stream resources. */
    pcdsp_stop_worker(pcdsp);

    /* (Re-)allocate the ring buffer.  capacity must be a power of two and
     * large enough to hold at least buffer_size frames. */
    size_t rb_cap = 1;
    while (rb_cap < io->buffer_size * RB_PERIODS)
        rb_cap <<= 1;

    pcdsp_rb_free(&pcdsp->rb);
    rc = pcdsp_rb_init(&pcdsp->rb, rb_cap, pcdsp->frame_bytes);
    if (rc < 0)
        return rc;

    pcdsp_timer_init(&pcdsp->timer, io->rate);
    atomic_store_explicit(&pcdsp->hw_frames, 0, memory_order_release);

    /*
     * Gate 7: START / READY handshake.
     *
     * Now that ALSA has negotiated exact stream parameters, connect to the
     * Rust controller, send HELLO + START, and wait for READY (or ERROR).
     * hw_params fails if the controller is unavailable or rejects the config.
     *
     * Invariant: no PCM must be transferred before READY is received.
     */
    pcdsp_ipc_close(&pcdsp->conn);

    /* Close any pipe fd from a previous stream before starting a new one. */
    if (pcdsp->pipe_fd >= 0) {
        close(pcdsp->pipe_fd);
        pcdsp->pipe_fd = -1;
    }

    int ipc_rc = pcdsp_ipc_connect(
        &pcdsp->conn,
        pcdsp->socket_path[0] ? pcdsp->socket_path : NULL);
    if (ipc_rc < 0) {
        SNDERR("picoredsp: controller unavailable (%d): %s",
               -ipc_rc, strerror(-ipc_rc));
        return ipc_rc;
    }

    ipc_rc = pcdsp_ipc_send_start(
        &pcdsp->conn,
        (uint32_t)io->rate,
        (uint8_t)io->format,
        (uint8_t)io->channels);
    if (ipc_rc < 0) {
        SNDERR("picoredsp: failed to send START (%d)", -ipc_rc);
        pcdsp_ipc_close(&pcdsp->conn);
        return ipc_rc;
    }

    pcdsp_error_code_t err_code = PCDSP_ERR_OK;
    /* Gate 8: pass &pcdsp->pipe_fd to receive the pipe write-end via
     * SCM_RIGHTS.  The controller sends it alongside the READY message. */
    ipc_rc = pcdsp_ipc_recv_ready(&pcdsp->conn, &pcdsp->pipe_fd, &err_code);
    if (ipc_rc < 0) {
        int result = ipc_rc;
        if (ipc_rc == -EPROTO && err_code != PCDSP_ERR_OK) {
            SNDERR("picoredsp: controller rejected stream (error code %d)", (int)err_code);
            switch (err_code) {
            case PCDSP_ERR_CONFIG:
                result = -EINVAL;
                break;
            case PCDSP_ERR_PLAYBACK_DEVICE:
                result = -ENODEV;
                break;
            case PCDSP_ERR_PROTOCOL:
                result = -EPROTO;
                break;
            case PCDSP_ERR_INTERNAL:
                result = -EIO;
                break;
            case PCDSP_ERR_OK:
            default:
                result = -EPROTO;
                break;
            }
        } else {
            SNDERR("picoredsp: failed waiting for READY (%d): %s",
                   -ipc_rc, strerror(-ipc_rc));
        }
        pcdsp_ipc_close(&pcdsp->conn);
        return result;
    }

    /* A blocking pipe write can hang shutdown indefinitely if CamillaDSP
     * remains alive but stops consuming stdin.  Keep the worker fd
     * non-blocking; pcm_worker.c polls in bounded intervals and observes the
     * atomic worker_running flag. */
    int pipe_flags = fcntl(pcdsp->pipe_fd, F_GETFL, 0);
    if (pipe_flags < 0 ||
        fcntl(pcdsp->pipe_fd, F_SETFL, pipe_flags | O_NONBLOCK) < 0) {
        int e = errno;
        close(pcdsp->pipe_fd);
        pcdsp->pipe_fd = -1;
        pcdsp_ipc_close(&pcdsp->conn);
        return -e;
    }

    return 0;
}

static int pcdsp_hw_free(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);
    pcdsp_stop_worker(pcdsp);
    pcdsp_rb_free(&pcdsp->rb);
    /* Release stream resources acquired by hw_params().  If the application
     * calls hw_free() without a preceding stop/drain (e.g. to renegotiate
     * hw_params), closing the pipe and IPC connection here ensures the Rust
     * controller is not left waiting for a STOP that will never arrive.
     * The controller treats an unexpected disconnect as a clean stream end. */
    if (pcdsp->pipe_fd >= 0) {
        close(pcdsp->pipe_fd);
        pcdsp->pipe_fd = -1;
    }
    pcdsp_ipc_close(&pcdsp->conn);
    return 0;
}

/*
 * sw_params — capture avail_min (and validate boundary) so poll_revents()
 * gates readiness the same way a real ALSA driver does: the application is
 * only woken once at least avail_min frames are free, not as soon as any
 * single frame is. Called by alsa-lib whenever the application configures
 * (or reconfigures) software parameters via snd_pcm_sw_params(); optional,
 * so applications that never call it keep the wake-on-any-space default set
 * in pcdsp_hw_params()/plugin open.
 */
static int pcdsp_sw_params(snd_pcm_ioplug_t *io, snd_pcm_sw_params_t *params)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    snd_pcm_uframes_t avail_min = 0;
    int rc = snd_pcm_sw_params_get_avail_min(params, &avail_min);
    if (rc == 0 && avail_min > 0)
        pcdsp->avail_min = avail_min;

    return 0;
}

static int pcdsp_prepare(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    /* Matching BlueALSA's bluealsa_prepare(): once the worker has recorded
     * a fatal stream error (the pipe to CamillaDSP is broken and cannot be
     * replaced without a fresh IPC handshake, which only hw_params()
     * performs), refuse to silently "recover" via prepare(). Clearing
     * stream_error here and returning success would let the application
     * believe the stream is healthy again while writes keep hitting the
     * same broken pipe_fd, immediately re-failing. Instead, surface
     * DISCONNECTED so the application is told to close() and reopen. */
    int serr = pcdsp_disconnect_on_stream_error(io, pcdsp);
    if (serr != 0)
        return serr;

    pcdsp_rb_reset(&pcdsp->rb);
    atomic_store_explicit(&pcdsp->hw_frames, 0, memory_order_release);
    /* stream_error is already known 0 here (checked above); store it
     * explicitly anyway as defence-in-depth against a late worker write
     * racing this reset. */
    atomic_store_explicit(&pcdsp->stream_error, 0, memory_order_release);
    atomic_store_explicit(&pcdsp->draining, false, memory_order_release);

    /* Drain eventfd from previous run. */
    uint64_t dummy;
    while (read(pcdsp->event_fd, &dummy, sizeof(dummy)) > 0)
        ;

    /* Playback clients that poll() before the start threshold is reached must
     * still see the PCM as writable immediately after prepare().  Arm the
     * eventfd once here; poll_revents() re-arms it while the PCM remains ready. */
    if (io->stream == SND_PCM_STREAM_PLAYBACK &&
        pcdsp_rb_write_avail(&pcdsp->rb) > 0)
        pcdsp_signal_event_fd(pcdsp->event_fd);

    return 0;
}

/*
 * pcdsp_drain_timeout_ns — bound for pcdsp_drain()'s wait, computed the same
 * way BlueALSA's playback drain bounds its own wait in bluealsa-pcm.c:
 * `100ms + periods_remaining * period_time`, rather than a single flat
 * constant. A flat timeout either aborts a slow-but-healthy drain of a large,
 * mostly-full buffer too early, or leaves a nearly-empty buffer waiting far
 * longer than it should if CamillaDSP is genuinely wedged. Scaling the bound
 * to how much audio is actually left to drain avoids both failure modes.
 *
 * periods_remaining is computed once at drain() entry (ceiling division of
 * frames currently queued by period_size); period_time is period_size /
 * rate. Falls back to a fixed floor if rate or period_size is unexpectedly
 * zero (should not happen post-hw_params, but must not divide by zero).
 */
static uint64_t pcdsp_drain_timeout_ns(const pcdsp_pcm_t *pcdsp, size_t avail_frames)
{
    const uint64_t floor_ns = 100ULL * 1000000ULL; /* 100 ms, matches BlueALSA */

    unsigned int rate        = pcdsp->io.rate;
    size_t       period_size = pcdsp->period_size;
    if (rate == 0 || period_size == 0)
        return PCDSP_DRAIN_TIMEOUT_NS;

    size_t periods_remaining = (avail_frames + period_size - 1) / period_size;
    uint64_t period_time_ns  = (uint64_t)period_size * 1000000000ULL / rate;

    return floor_ns + (uint64_t)periods_remaining * period_time_ns;
}

static int pcdsp_drain(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    /* Signal the worker that a drain is in progress so it consumes partial
     * periods rather than waiting for a full one.  This prevents the
     * deadlock where the final partial period never reaches period_size. */
    atomic_store_explicit(&pcdsp->draining, true, memory_order_release);

    /* Bound the wait so a CamillaDSP that stops reading stdin (process
     * alive, pipe never returns POLLOUT/EPIPE) cannot block drain()
     * forever. pcdsp_clock_now() failure (should not happen in practice)
     * degrades to an unbounded wait rather than a false-positive timeout.
     * The bound itself scales with how much is left to drain (see
     * pcdsp_drain_timeout_ns()), matching BlueALSA's approach instead of a
     * flat ceiling. */
    uint64_t start_ns    = 0;
    bool     have_clock  = (pcdsp_clock_now(&start_ns) == 0);
    bool     timed_out   = false;
    uint64_t timeout_ns  = pcdsp_drain_timeout_ns(pcdsp, pcdsp_rb_read_avail(&pcdsp->rb));

    /* Wait until the ring buffer is empty, the worker stops (no consumer
     * → buffer will never drain), a fatal stream error is recorded, or the
     * timeout elapses. */
    while (pcdsp_rb_read_avail(&pcdsp->rb) > 0) {
        if (!atomic_load_explicit(&pcdsp->worker_running, memory_order_acquire))
            break;
        if (atomic_load_explicit(&pcdsp->stream_error, memory_order_acquire) != 0)
            break;
        if (have_clock) {
            uint64_t now_ns;
            if (pcdsp_clock_now(&now_ns) == 0 &&
                now_ns - start_ns >= timeout_ns) {
                timed_out = true;
                break;
            }
        }
        struct timespec ts = { .tv_nsec = 1000000 };
        nanosleep(&ts, NULL);
    }

    atomic_store_explicit(&pcdsp->draining, false, memory_order_release);

    /* Matching BlueALSA's bluealsa_drain(): if a fatal stream error was
     * recorded, transition to DISCONNECTED before returning it. */
    int serr = pcdsp_disconnect_on_stream_error(io, pcdsp);
    if (serr != 0)
        return serr;
    if (timed_out)
        return -ETIMEDOUT;
    if (pcdsp_rb_read_avail(&pcdsp->rb) > 0)
        return -EPIPE;
    return 0;
}

static int pcdsp_pause(snd_pcm_ioplug_t *io, int enable)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    /* Matching BlueALSA's bluealsa_pause(): refuse to pause or resume a
     * stream whose worker has recorded a fatal error, and make that
     * DISCONNECTED transition visible immediately rather than only on the
     * next pointer()-driven avail update. */
    int serr = pcdsp_disconnect_on_stream_error(io, pcdsp);
    if (serr != 0)
        return serr;

    if (!enable) {
        /* Resume: clear the flag and wake a worker parked in the pause
         * wait immediately; no ack is required to observe a resume. */
        pthread_mutex_lock(&pcdsp->pause_mutex);
        atomic_store_explicit(&pcdsp->paused, false, memory_order_release);
        pthread_cond_broadcast(&pcdsp->pause_cond);
        pthread_mutex_unlock(&pcdsp->pause_mutex);
        pcdsp_timer_start(&pcdsp->timer);
        return 0;
    }

    /* Pause: request the worker to park, then block until it has actually
     * acknowledged the pause (reached a safe point, not mid-write) or has
     * stopped running — so pause() cannot return successfully while a
     * write is still in flight. Bounded by PCDSP_PAUSE_ACK_TIMEOUT_NS in
     * case the worker is wedged for an unrelated reason; if the deadline
     * is reached without an acknowledgement while the worker is still
     * running, the "no write in flight" invariant could not be confirmed,
     * so this returns -ETIMEDOUT rather than reporting success. */
    pthread_mutex_lock(&pcdsp->pause_mutex);
    atomic_store_explicit(&pcdsp->paused, true, memory_order_release);

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec  += (time_t)(PCDSP_PAUSE_ACK_TIMEOUT_NS / 1000000000ULL);
    deadline.tv_nsec += (long)(PCDSP_PAUSE_ACK_TIMEOUT_NS % 1000000000ULL);
    if (deadline.tv_nsec >= 1000000000L) {
        deadline.tv_nsec -= 1000000000L;
        deadline.tv_sec  += 1;
    }

    int wait_rc = pcdsp_wait_pause_ack(&pcdsp->pause_mutex, &pcdsp->pause_cond,
                                        &pcdsp->worker_paused_ack,
                                        &pcdsp->worker_running, &deadline);
    if (wait_rc != 0) {
        /* Acknowledgement not confirmed within the deadline: retract the
         * pause request (under the same lock, so this cannot race the
         * worker's own ack) so the worker does not park later for a pause
         * the caller was just told had failed. */
        atomic_store_explicit(&pcdsp->paused, false, memory_order_release);
        pthread_mutex_unlock(&pcdsp->pause_mutex);
        return wait_rc;
    }
    pthread_mutex_unlock(&pcdsp->pause_mutex);

    pcdsp_timer_stop(&pcdsp->timer);
    return 0;
}

static int pcdsp_close(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    /* Stop and reap before changing pipe_fd; the worker's bounded non-blocking
     * write loop observes worker_running=false without relying on close(2) to
     * interrupt another thread's system call. */
    pcdsp_stop_worker(pcdsp);

    if (pcdsp->pipe_fd >= 0) {
        close(pcdsp->pipe_fd);
        pcdsp->pipe_fd = -1;
    }

    pcdsp_rb_free(&pcdsp->rb);
    pcdsp_ipc_close(&pcdsp->conn);

    if (pcdsp->event_fd >= 0) {
        close(pcdsp->event_fd);
        pcdsp->event_fd = -1;
    }

    pthread_mutex_destroy(&pcdsp->pause_mutex);
    pthread_cond_destroy(&pcdsp->pause_cond);

    free(pcdsp);
    return 0;
}

static int pcdsp_poll_descriptors_count(snd_pcm_ioplug_t *io)
{
    (void)io;
    return 1;
}

static int pcdsp_poll_descriptors(snd_pcm_ioplug_t *io,
                                   struct pollfd    *pfd,
                                   unsigned int      space)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);
    if (space < 1)
        return -EINVAL;
    pfd[0].fd     = pcdsp->event_fd;
    pfd[0].events = POLLIN;
    return 1;
}

static int pcdsp_poll_revents(snd_pcm_ioplug_t *io,
                               struct pollfd    *pfd,
                               unsigned int      nfds,
                               unsigned short   *revents)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);
    if (nfds < 1 || !pfd || !revents)
        return -EINVAL;

    *revents = 0;

    /* If the worker recorded a fatal stream error, this PCM instance cannot
     * recover on its own. Matching BlueALSA's poll_revents() `fail:` path:
     * proactively transition to DISCONNECTED (rather than leaving state
     * undecided) and report POLLERR|POLLHUP with -ENODEV, so the
     * application wakes from poll() and immediately sees a definitive,
     * non-recoverable error instead of one that could be mistaken for a
     * transient XRUN. */
    int serr = atomic_load_explicit(&pcdsp->stream_error, memory_order_acquire);
    if (serr != 0) {
        snd_pcm_ioplug_set_state(io, SND_PCM_STATE_DISCONNECTED);
        *revents = POLLERR | POLLHUP;
        return -ENODEV;
    }

    if (pfd[0].revents & POLLIN) {
        /* Consume the eventfd counter. */
        uint64_t val;
        if (read(pfd[0].fd, &val, sizeof(val)) > 0) {
            snd_pcm_sframes_t avail = snd_pcm_avail(io->pcm);
            bool ready = false;

            if (avail < 0) {
                *revents = POLLERR;
                return 0;
            }

            switch (io->state) {
            case SND_PCM_STATE_SETUP:
            case SND_PCM_STATE_PREPARED:
                ready = io->stream == SND_PCM_STREAM_PLAYBACK;
                break;
            case SND_PCM_STATE_RUNNING:
                /* Gate readiness on avail_min (sw_params), matching real
                 * ALSA driver semantics, instead of waking as soon as a
                 * single frame is free. */
                if (io->stream == SND_PCM_STREAM_PLAYBACK)
                    ready = (snd_pcm_uframes_t)avail >= pcdsp->avail_min;
                break;
            case SND_PCM_STATE_DRAINING:
                /* Keep playback wakeups level-triggered until the last
                 * buffered frames have been consumed, so blocking writers and
                 * drain completion do not stall after a one-shot eventfd edge. */
                if (io->stream == SND_PCM_STREAM_PLAYBACK)
                    ready = pcdsp_rb_read_avail(&pcdsp->rb) == 0;
                break;
            case SND_PCM_STATE_XRUN:
            case SND_PCM_STATE_PAUSED:
            case SND_PCM_STATE_SUSPENDED:
                *revents = POLLERR;
                break;
            case SND_PCM_STATE_OPEN:
                *revents = POLLERR;
                return -EBADF;
            case SND_PCM_STATE_DISCONNECTED:
                *revents = POLLERR | POLLHUP;
                return -ENODEV;
            default:
                break;
            }

            if (ready) {
                *revents = io->stream == SND_PCM_STREAM_CAPTURE ? POLLIN : POLLOUT;
                /* eventfd is edge-triggered; re-arm it while the PCM remains
                 * writable so ALSA observes level-triggered readiness. */
                pcdsp_signal_event_fd(pcdsp->event_fd);
            }
        }
    }
    return 0;
}

static int pcdsp_delay(snd_pcm_ioplug_t *io, snd_pcm_sframes_t *delayp)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);
    /* Matching BlueALSA's bluealsa_delay(): transition to DISCONNECTED
     * before reporting the error, so the state is visible even if the
     * application only ever calls delay() and never pointer(). */
    int serr = pcdsp_disconnect_on_stream_error(io, pcdsp);
    if (serr != 0)
        return serr;

    /* Frames written by the application but not yet consumed by the worker. */
    snd_pcm_sframes_t delay = (snd_pcm_sframes_t)pcdsp_rb_read_avail(&pcdsp->rb);

    /* Frames already handed to the kernel pipe but not yet read by
     * CamillaDSP are also audio that hasn't reached the DAC yet. FIONREAD
     * reports the pipe's current queued byte count; add the equivalent
     * frame count as a minimum additional delay.
     *
     * BlueALSA avoids this syscall on every delay() call by snapshotting
     * the FIFO occupancy from its IO thread and extrapolating with elapsed
     * time in bluealsa_calculate_delay(). A direct (measured, not tried)
     * port of that technique was evaluated here and reverted: it assumes
     * the downstream peer keeps consuming at the nominal rate between
     * snapshots, which silently under-reports delay once a wedged/stalled
     * CamillaDSP stops reading stdin — exactly the scenario
     * `delay_accounts_for_frames_queued_in_kernel_pipe` (below) exists to
     * guard against, and unlike BlueALSA's Bluetooth transport, a stalled
     * peer is a first-class, explicitly-tested case for this plugin (see
     * also the Drain and Pause tests). Re-querying the kernel on every call
     * keeps that guarantee exact rather than trading it for a syscall
     * saving BlueALSA's own use case doesn't need us to make.
     *
     * Known limitation: this still does not account for CamillaDSP's own
     * internal buffering (resampler/filter/pipeline latency) once bytes
     * leave the pipe — the plugin has no visibility into that from the
     * transport side. See docs/BLUEALSA_TRACKING.md "Delay accounting". */
    if (pcdsp->pipe_fd >= 0 && pcdsp->frame_bytes > 0) {
        int queued_bytes = 0;
        if (ioctl(pcdsp->pipe_fd, FIONREAD, &queued_bytes) == 0 && queued_bytes > 0)
            delay += (snd_pcm_sframes_t)((size_t)queued_bytes / pcdsp->frame_bytes);
    }

    *delayp = delay;
    return 0;
}

static void pcdsp_dump(snd_pcm_ioplug_t *io, snd_output_t *out)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);
    snd_output_printf(out, "piCoreDSP ioplug (Gate 8 stdin pipe transport)\n");
    snd_output_printf(out, "  rate: %u Hz, channels: %u, format: %s\n",
                      io->rate, io->channels,
                      snd_pcm_format_name(io->format));
    snd_output_printf(out, "  socket: %s\n",
                      pcdsp->socket_path[0] ? pcdsp->socket_path
                                            : PCDSP_IPC_DEFAULT_SOCKET_PATH);
}

/* -----------------------------------------------------------------------
 * Callback table
 * ---------------------------------------------------------------------- */

static const snd_pcm_ioplug_callback_t pcdsp_callbacks = {
    .start                  = pcdsp_start,
    .stop                   = pcdsp_stop,
    .pointer                = pcdsp_pointer,
    .transfer               = pcdsp_transfer,
    .close                  = pcdsp_close,
    .hw_params              = pcdsp_hw_params,
    .hw_free                = pcdsp_hw_free,
    .sw_params              = pcdsp_sw_params,
    .prepare                = pcdsp_prepare,
    .drain                  = pcdsp_drain,
    .pause                  = pcdsp_pause,
    .poll_descriptors_count = pcdsp_poll_descriptors_count,
    .poll_descriptors       = pcdsp_poll_descriptors,
    .poll_revents           = pcdsp_poll_revents,
    .delay                  = pcdsp_delay,
    .dump                   = pcdsp_dump,
};

/* -----------------------------------------------------------------------
 * Plugin entry point
 * ---------------------------------------------------------------------- */

/*
 * SND_PCM_PLUGIN_DEFINE_FUNC — the symbol that ALSA looks up when loading
 * the plugin, e.g. when the ALSA config contains:
 *
 *   pcm.picoredsp {
 *       type picoredsp
 *   }
 */
SND_PCM_PLUGIN_DEFINE_FUNC(picoredsp)
{
    (void)root;
    (void)name;

    pcdsp_pcm_t *pcdsp = calloc(1, sizeof(*pcdsp));
    if (!pcdsp)
        return -ENOMEM;

    pcdsp->event_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (pcdsp->event_fd < 0) {
        int e = errno;
        free(pcdsp);
        return -e;
    }

    pcdsp->conn.fd                 = -1;
    pcdsp->conn.negotiated_version = 0;
    pcdsp->pipe_fd                 = -1;
    pcdsp->avail_min                = 1;

    atomic_init(&pcdsp->worker_running, false);
    pcdsp->worker_joinable = false;
    atomic_init(&pcdsp->paused,         false);
    atomic_init(&pcdsp->draining,       false);
    atomic_init(&pcdsp->stream_error,   0);
    atomic_init(&pcdsp->hw_frames,      0);
    pcdsp->worker_paused_ack = false;

    int mutex_rc = pthread_mutex_init(&pcdsp->pause_mutex, NULL);
    if (mutex_rc != 0) {
        close(pcdsp->event_fd);
        free(pcdsp);
        return -mutex_rc;
    }
    int cond_rc = pthread_cond_init(&pcdsp->pause_cond, NULL);
    if (cond_rc != 0) {
        pthread_mutex_destroy(&pcdsp->pause_mutex);
        close(pcdsp->event_fd);
        free(pcdsp);
        return -cond_rc;
    }

    /* Parse optional ALSA config parameters. */
    const char *socket_path = NULL;
    snd_config_t *n;
    snd_config_iterator_t i, next;
    snd_config_for_each(i, next, conf) {
        n = snd_config_iterator_entry(i);
        const char *id;
        if (snd_config_get_id(n, &id) < 0)
            continue;
        if (strcmp(id, "type") == 0 || strcmp(id, "comment") == 0 ||
            strcmp(id, "hint") == 0)
            continue;
        if (strcmp(id, "socket_path") == 0) {
            snd_config_get_string(n, &socket_path);
            continue;
        }
        /* Unknown key — warn but do not fail. */
        SNDERR("picoredsp: unknown config key '%s'", id);
    }

    /* Store socket path for IPC (Gate 7+).  Reject overlong paths instead of
     * silently truncating them to a different AF_UNIX endpoint. */
    if (socket_path) {
        size_t socket_path_len = strlen(socket_path);
        if (socket_path_len >= sizeof(pcdsp->socket_path)) {
            pthread_cond_destroy(&pcdsp->pause_cond);
            pthread_mutex_destroy(&pcdsp->pause_mutex);
            close(pcdsp->event_fd);
            free(pcdsp);
            return -ENAMETOOLONG;
        }
        memcpy(pcdsp->socket_path, socket_path, socket_path_len + 1); /* NOLINT(clang-analyzer-security.insecureAPI.DeprecatedOrUnsafeBufferHandling) */
    }
    /* else socket_path[0] == '\0' (zero-initialised by calloc) → use default */

    pcdsp->io.version      = SND_PCM_IOPLUG_VERSION;
    pcdsp->io.name         = "piCoreDSP ioplug";
    pcdsp->io.flags        = SND_PCM_IOPLUG_FLAG_LISTED | SND_PCM_IOPLUG_FLAG_MONOTONIC;
#ifdef SND_PCM_IOPLUG_FLAG_BOUNDARY_WA
    pcdsp->io.flags       |= SND_PCM_IOPLUG_FLAG_BOUNDARY_WA;
#endif
    /* The plugin provides an explicit transfer() callback and keeps its own
     * ring buffer. Pseudo-mmap mode bypasses that callback and leaves the
     * worker with no frames to drain, so keep mmap emulation disabled. */
    pcdsp->io.mmap_rw      = 0;
    pcdsp->io.callback     = &pcdsp_callbacks;
    pcdsp->io.private_data = pcdsp;

    int rc = snd_pcm_ioplug_create(&pcdsp->io, name, stream, mode);
    if (rc < 0) {
        pthread_cond_destroy(&pcdsp->pause_cond);
        pthread_mutex_destroy(&pcdsp->pause_mutex);
        close(pcdsp->event_fd);
        free(pcdsp);
        return rc;
    }

    /* Set hw constraints. */
    static const unsigned int access_list[] = {
        SND_PCM_ACCESS_RW_INTERLEAVED,
    };
    snd_pcm_ioplug_set_param_list(&pcdsp->io, SND_PCM_IOPLUG_HW_ACCESS,
                                  1, access_list);

    unsigned int fmt_list[16];
    size_t       fmt_count = pcdsp_format_list(fmt_list, 16);
    snd_pcm_ioplug_set_param_list(&pcdsp->io, SND_PCM_IOPLUG_HW_FORMAT,
                                  (unsigned int)fmt_count, fmt_list);

    /* Constrain the plugin to stereo only.  The documented product contract
     * is stereo (2 channels) and the stack buffer in the worker is sized for
     * PCDSP_MAX_CHANNELS = 2.  Advertising 1..8 here was unsafe (stack
     * overflow with 8ch × 4 bytes/frame > 2-byte/frame assumption). */
    snd_pcm_ioplug_set_param_minmax(&pcdsp->io, SND_PCM_IOPLUG_HW_CHANNELS,
                                    2, 2);

    snd_pcm_ioplug_set_param_list(&pcdsp->io, SND_PCM_IOPLUG_HW_RATE,
                                  sizeof(k_rates) / sizeof(k_rates[0]), k_rates);

    snd_pcm_ioplug_set_param_minmax(&pcdsp->io, SND_PCM_IOPLUG_HW_PERIOD_BYTES,
                                    PERIOD_SIZE_MIN * 2,              /* 64 × 2ch × 1B */
                                    PERIOD_SIZE_MAX * 2 * 4);         /* 8192 × 2ch × 4B */

    snd_pcm_ioplug_set_param_minmax(&pcdsp->io, SND_PCM_IOPLUG_HW_PERIODS,
                                    2, RB_PERIODS);

    *pcmp = pcdsp->io.pcm;
    return 0;
}

SND_PCM_PLUGIN_SYMBOL(picoredsp);
