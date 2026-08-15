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
 *   ✓ stop sends STOP to the controller
 *
 * Gate 8 adds the stdin pipe transport:
 *   ✓ Rust controller creates a pipe, spawns CamillaDSP with read-end as stdin
 *   ✓ Rust sends READY with the pipe write-end via SCM_RIGHTS
 *   ✓ plugin receives the write-end fd in pcdsp_ipc_recv_ready()
 *   ✓ worker thread drains the ring buffer by writing to the pipe fd
 *   ✓ on STOP / disconnect the plugin closes the fd, Rust also closes its copy
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

/* -----------------------------------------------------------------------
 * Plugin private data
 * ---------------------------------------------------------------------- */

typedef struct pcdsp_pcm {
    snd_pcm_ioplug_t    io;          /* MUST be first — cast back via container_of */

    /* hw_params-negotiated values */
    size_t              frame_bytes;
    snd_pcm_uframes_t   period_size;
    snd_pcm_uframes_t   buffer_size;

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

    while (atomic_load_explicit(&pcdsp->worker_running, memory_order_acquire)) {
        if (atomic_load_explicit(&pcdsp->paused, memory_order_acquire)) {
            struct timespec ts = { .tv_sec = 0, .tv_nsec = 1000000 }; /* 1 ms */
            nanosleep(&ts, NULL);
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

    /* Gate 7: notify the controller that the stream is ending. */
    if (pcdsp->conn.fd >= 0)
        pcdsp_ipc_send_stop(&pcdsp->conn);

    /* Stop/reap the worker before closing pipe_fd.  The worker uses bounded
     * non-blocking pipe writes, so it observes worker_running=false promptly. */
    pcdsp_stop_worker(pcdsp);
    pcdsp_timer_stop(&pcdsp->timer);

    /* Gate 8: close the pipe write-end so CamillaDSP sees EOF once Rust
     * also closes its copy. */
    if (pcdsp->pipe_fd >= 0) {
        close(pcdsp->pipe_fd);
        pcdsp->pipe_fd = -1;
    }

    /* Drain eventfd. */
    uint64_t dummy;
    while (read(pcdsp->event_fd, &dummy, sizeof(dummy)) > 0)
        ;

    return 0;
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

    /* If the worker recorded a fatal stream error (e.g. -EPIPE when
     * CamillaDSP exited), return it immediately so ALSA sees the error. */
    int serr = atomic_load_explicit(&pcdsp->stream_error, memory_order_acquire);
    if (serr != 0)
        return (snd_pcm_sframes_t)serr;

    uint64_t hw_total = atomic_load_explicit(&pcdsp->hw_frames, memory_order_acquire);
    snd_pcm_sframes_t hw_ptr = (snd_pcm_sframes_t)hw_total;
#ifndef SND_PCM_IOPLUG_FLAG_BOUNDARY_WA
    hw_ptr %= (snd_pcm_sframes_t)pcdsp->buffer_size;
#endif

    /* Check for XRUN: if the application has written more than buffer_size
     * frames ahead of what the consumer has drained, declare XRUN. */
    uint64_t rp = atomic_load_explicit(&pcdsp->rb.read_pos, memory_order_acquire);
    uint64_t wp = atomic_load_explicit(&pcdsp->rb.write_pos, memory_order_acquire);
    if ((wp - rp) > pcdsp->buffer_size)
        return -EPIPE;

    return (snd_pcm_sframes_t)hw_ptr;
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

static int pcdsp_prepare(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);
    pcdsp_rb_reset(&pcdsp->rb);
    atomic_store_explicit(&pcdsp->hw_frames, 0, memory_order_release);
    /* Clear any error/drain state from a previous run. */
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

static int pcdsp_drain(snd_pcm_ioplug_t *io)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);

    /* Signal the worker that a drain is in progress so it consumes partial
     * periods rather than waiting for a full one.  This prevents the
     * deadlock where the final partial period never reaches period_size. */
    atomic_store_explicit(&pcdsp->draining, true, memory_order_release);

    /* Wait until the ring buffer is empty or the worker stops (no consumer
     * → buffer will never drain). */
    while (pcdsp_rb_read_avail(&pcdsp->rb) > 0) {
        if (!atomic_load_explicit(&pcdsp->worker_running, memory_order_acquire))
            break;
        struct timespec ts = { .tv_nsec = 1000000 };
        nanosleep(&ts, NULL);
    }

    atomic_store_explicit(&pcdsp->draining, false, memory_order_release);

    int serr = atomic_load_explicit(&pcdsp->stream_error, memory_order_acquire);
    if (serr != 0)
        return serr;
    if (pcdsp_rb_read_avail(&pcdsp->rb) > 0)
        return -EPIPE;
    return 0;
}

static int pcdsp_pause(snd_pcm_ioplug_t *io, int enable)
{
    pcdsp_pcm_t *pcdsp = io_to_pcdsp(io);
    atomic_store_explicit(&pcdsp->paused, (bool)enable, memory_order_release);
    if (!enable)
        pcdsp_timer_start(&pcdsp->timer);
    else
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

    /* If the worker recorded a fatal stream error, expose it as POLLERR so
     * the application wakes from poll() and subsequently sees the error in
     * pointer() or delay(). */
    int serr = atomic_load_explicit(&pcdsp->stream_error, memory_order_acquire);
    if (serr != 0) {
        *revents = POLLERR;
        return 0;
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
                if (io->stream == SND_PCM_STREAM_PLAYBACK)
                    ready = (snd_pcm_uframes_t)avail > 0;
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
    int serr = atomic_load_explicit(&pcdsp->stream_error, memory_order_acquire);
    if (serr != 0)
        return serr;
    /* Delay = frames currently in the ring buffer awaiting consumption. */
    *delayp = (snd_pcm_sframes_t)pcdsp_rb_read_avail(&pcdsp->rb);
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

    atomic_init(&pcdsp->worker_running, false);
    pcdsp->worker_joinable = false;
    atomic_init(&pcdsp->paused,         false);
    atomic_init(&pcdsp->draining,       false);
    atomic_init(&pcdsp->stream_error,   0);
    atomic_init(&pcdsp->hw_frames,      0);

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
