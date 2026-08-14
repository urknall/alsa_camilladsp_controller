/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * timing.c — stream timing helpers
 *
 * The ioplug transfer callback is responsible for advancing hw_ptr by the
 * number of frames consumed in each period.  This file provides helpers
 * for computing the current hardware read position from monotonic time and
 * the nominal sample rate so that `pointer()` can return a value that
 * satisfies ALSA timing expectations without a real DMA device.
 *
 * In the null-sink prototype (Gate 4/M4) the worker thread drains from the
 * ring buffer at the nominal rate using these helpers.  When the real data
 * path (pipe → CamillaDSP) is wired up in Gate 8, the consumer simply
 * writes to the pipe and advances hw_ptr by the frames written.
 */

#include "timing.h"

#include <errno.h>
#include <time.h>

/* -----------------------------------------------------------------------
 * Monotonic clock helper
 * ---------------------------------------------------------------------- */

int pcdsp_clock_now(uint64_t *ns_out)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) < 0)
        return -errno;
    *ns_out = (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
    return 0;
}

/* -----------------------------------------------------------------------
 * Stream timer
 * ---------------------------------------------------------------------- */

void pcdsp_timer_init(pcdsp_stream_timer_t *t, unsigned int rate)
{
    t->start_ns   = 0;
    t->rate       = rate;
    t->running    = 0;
}

void pcdsp_timer_start(pcdsp_stream_timer_t *t)
{
    uint64_t now = 0;
    if (pcdsp_clock_now(&now) == 0) {
        t->start_ns = now;
        t->running  = 1;
    }
}

void pcdsp_timer_stop(pcdsp_stream_timer_t *t)
{
    t->running = 0;
}

/*
 * pcdsp_timer_elapsed_frames — frames elapsed since the timer was started,
 * based on the nominal sample rate.
 *
 * This is used by the null-sink pointer() implementation to synthesise a
 * monotonically increasing hw_ptr without a real DMA pointer.
 */
uint64_t pcdsp_timer_elapsed_frames(const pcdsp_stream_timer_t *t)
{
    if (!t->running || t->rate == 0)
        return 0;

    uint64_t now = 0;
    if (pcdsp_clock_now(&now) < 0)
        return 0;

    if (now < t->start_ns)
        return 0;

    /* frames = elapsed_ns * rate / 1e9
     * Split into seconds and sub-second parts to avoid overflow and drift:
     *   whole_seconds * rate + (sub_ns * rate / 1_000_000_000)
     * sub_ns < 1e9 and rate ≤ 192000, so sub_ns * rate < 1.92e14 which
     * fits comfortably in uint64_t. */
    uint64_t elapsed_ns   = now - t->start_ns;
    uint64_t whole_secs   = elapsed_ns / 1000000000ULL;
    uint64_t sub_ns       = elapsed_ns % 1000000000ULL;
    return whole_secs * t->rate + sub_ns * t->rate / 1000000000ULL;
}
