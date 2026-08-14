/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * bench/bench_timing.c — timing module microbenchmarks
 *
 * Measures:
 *   clock_now_overhead          nanoseconds per pcdsp_clock_now() call
 *   clock_consecutive_delta     minimum observed delta between two calls
 *   elapsed_frames_overhead     nanoseconds per pcdsp_timer_elapsed_frames()
 *   timer_accuracy_50ms         compare timer-derived frames vs actual sleep
 *   timer_accuracy_200ms        same over 200 ms (lower relative error expected)
 */

#include "bench_harness.h"
#include "timing.h"

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>

#define ITERS  10000
#define WARMUP   500

/* -----------------------------------------------------------------------
 * clock_now overhead
 * ---------------------------------------------------------------------- */

static void bench_clock_now_overhead(void)
{
    bench_ctx_t ctx;
    bench_ctx_init(&ctx, "clock_now_overhead (pcdsp_clock_now)", ITERS);

    uint64_t t;
    for (int i = 0; i < WARMUP; i++) pcdsp_clock_now(&t);

    for (int i = 0; i < ctx.capacity; i++) {
        bench_iter_start(&ctx);
        pcdsp_clock_now(&t);
        bench_iter_end(&ctx);
    }

    bench_ctx_report(&ctx);
    bench_ctx_free(&ctx);
}

/* -----------------------------------------------------------------------
 * Minimum observable clock delta
 *
 * Calls pcdsp_clock_now twice back-to-back and records the difference.
 * The minimum non-zero delta is the effective clock resolution.
 * ---------------------------------------------------------------------- */

static void bench_clock_consecutive_delta(void)
{
    const int n = ITERS;
    uint64_t min_delta = UINT64_MAX;
    uint64_t sum_delta = 0;
    int nonzero = 0;

    for (int w = 0; w < WARMUP; w++) {
        uint64_t a, b;
        pcdsp_clock_now(&a);
        pcdsp_clock_now(&b);
        (void)(b - a);
    }

    for (int i = 0; i < n; i++) {
        uint64_t a, b;
        pcdsp_clock_now(&a);
        pcdsp_clock_now(&b);
        uint64_t delta = b - a;
        if (delta > 0) {
            if (delta < min_delta) min_delta = delta;
            sum_delta += delta;
            nonzero++;
        }
    }

    double mean_ns = nonzero > 0 ? (double)sum_delta / (double)nonzero : 0.0;

    printf("  %-52s  n=%-6d  min_nonzero=%5" PRIu64 " ns  mean_nonzero=%5.0f ns"
           "  zero_count=%d\n",
           "clock_consecutive_delta",
           n, min_delta == UINT64_MAX ? 0 : min_delta, mean_ns, n - nonzero);
}

/* -----------------------------------------------------------------------
 * elapsed_frames overhead
 * ---------------------------------------------------------------------- */

static void bench_elapsed_frames_overhead(void)
{
    bench_ctx_t ctx;
    bench_ctx_init(&ctx, "elapsed_frames_overhead (timer running, 48 kHz)", ITERS);

    pcdsp_stream_timer_t t;
    pcdsp_timer_init(&t, 48000);
    pcdsp_timer_start(&t);

    for (int i = 0; i < WARMUP; i++) pcdsp_timer_elapsed_frames(&t);

    for (int i = 0; i < ctx.capacity; i++) {
        bench_iter_start(&ctx);
        pcdsp_timer_elapsed_frames(&t);
        bench_iter_end(&ctx);
    }

    pcdsp_timer_stop(&t);
    bench_ctx_report(&ctx);
    bench_ctx_free(&ctx);
}

/* -----------------------------------------------------------------------
 * Timer accuracy
 *
 * Sleep for `sleep_ms` milliseconds, then compare the expected number of
 * frames (rate * sleep_ms / 1000) to pcdsp_timer_elapsed_frames().
 *
 * Reports:
 *   expected_frames  — nominal value from the sleep duration
 *   measured_frames  — value returned by pcdsp_timer_elapsed_frames
 *   error_frames     — measured − expected
 *   error_ppm        — parts-per-million relative error
 * ---------------------------------------------------------------------- */

static void bench_timer_accuracy(const char *name, unsigned int rate, long sleep_ms)
{
    struct timespec req = {
        .tv_sec  = sleep_ms / 1000,
        .tv_nsec = (sleep_ms % 1000) * 1000000L,
    };

    pcdsp_stream_timer_t t;
    pcdsp_timer_init(&t, rate);
    pcdsp_timer_start(&t);
    nanosleep(&req, NULL);
    uint64_t measured = pcdsp_timer_elapsed_frames(&t);
    pcdsp_timer_stop(&t);

    double expected = (double)rate * (double)sleep_ms / 1000.0;
    double error    = (double)measured - expected;
    double ppm      = expected > 0.0 ? (error / expected) * 1e6 : 0.0;

    printf("  %-52s  rate=%6u Hz  sleep_ms=%4ld  expected=%.0f frames"
           "  measured=%" PRIu64 " frames  error=%+.0f frames  error=%+.0f ppm\n",
           name, rate, sleep_ms, expected, measured, error, ppm);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("bench_timing\n");

    bench_section("Clock overhead");
    bench_clock_now_overhead();
    bench_clock_consecutive_delta();

    bench_section("Timer overhead");
    bench_elapsed_frames_overhead();

    bench_section("Timer accuracy (sleep-based)");
    bench_timer_accuracy("timer_accuracy_50ms  @ 44100 Hz",  44100,  50);
    bench_timer_accuracy("timer_accuracy_50ms  @ 48000 Hz",  48000,  50);
    bench_timer_accuracy("timer_accuracy_50ms  @ 96000 Hz",  96000,  50);
    bench_timer_accuracy("timer_accuracy_50ms  @ 192000 Hz", 192000, 50);
    bench_timer_accuracy("timer_accuracy_200ms @ 48000 Hz",  48000, 200);
    bench_timer_accuracy("timer_accuracy_200ms @ 96000 Hz",  96000, 200);

    printf("\nbench_timing done\n");
    return 0;
}
