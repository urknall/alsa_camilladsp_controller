/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * bench/bench_harness.h — lightweight benchmark framework
 *
 * Provides a minimal statistics engine for C microbenchmarks:
 *   - CLOCK_MONOTONIC timing via bench_clock_ns()
 *   - Per-iteration measurement with bench_iter_start / bench_iter_end
 *   - Sort-based percentile statistics (min, p50, p95, p99, max, mean, stddev)
 *   - Human-readable console output via bench_ctx_report()
 *   - Throughput helper: bench_ctx_report_throughput()
 *
 * Usage pattern:
 *
 *   bench_ctx_t ctx;
 *   bench_ctx_init(&ctx, "my_bench", 10000);
 *
 *   // Optional warm-up (not recorded)
 *   for (int i = 0; i < 200; i++) { do_work(); }
 *
 *   // Measured iterations
 *   for (int i = 0; i < ctx.capacity; i++) {
 *       bench_iter_start(&ctx);
 *       do_work();
 *       bench_iter_end(&ctx);
 *   }
 *
 *   bench_ctx_report(&ctx);
 *   bench_ctx_free(&ctx);
 */

#ifndef BENCH_HARNESS_H
#define BENCH_HARNESS_H

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* -----------------------------------------------------------------------
 * Types
 * ---------------------------------------------------------------------- */

typedef struct {
    const char *name;
    uint64_t   *samples;   /* heap-allocated array of per-iteration ns durations */
    int         capacity;  /* maximum number of samples (== requested iters)     */
    int         count;     /* samples recorded so far                            */
    uint64_t    t0;        /* wall-clock ns at bench_iter_start                  */
} bench_ctx_t;

/* -----------------------------------------------------------------------
 * Clock
 * ---------------------------------------------------------------------- */

static inline uint64_t bench_clock_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

/* -----------------------------------------------------------------------
 * Lifecycle
 * ---------------------------------------------------------------------- */

static inline void bench_ctx_init(bench_ctx_t *ctx, const char *name, int iters)
{
    ctx->name     = name;
    ctx->capacity = iters;
    ctx->count    = 0;
    ctx->t0       = 0;
    ctx->samples  = (uint64_t *)malloc((size_t)iters * sizeof(uint64_t));
    if (!ctx->samples) {
        fprintf(stderr, "bench_harness: out of memory allocating %d samples\n", iters);
        exit(1);
    }
}

static inline void bench_ctx_free(bench_ctx_t *ctx)
{
    free(ctx->samples);
    ctx->samples  = NULL;
    ctx->capacity = 0;
    ctx->count    = 0;
}

/* -----------------------------------------------------------------------
 * Per-iteration measurement
 * ---------------------------------------------------------------------- */

static inline void bench_iter_start(bench_ctx_t *ctx)
{
    ctx->t0 = bench_clock_ns();
}

static inline void bench_iter_end(bench_ctx_t *ctx)
{
    uint64_t t1 = bench_clock_ns();
    if (ctx->count < ctx->capacity)
        ctx->samples[ctx->count++] = t1 - ctx->t0;
}

/* -----------------------------------------------------------------------
 * Statistics helpers
 * ---------------------------------------------------------------------- */

static int bench_cmp_u64(const void *a, const void *b)
{
    uint64_t x = *(const uint64_t *)a;
    uint64_t y = *(const uint64_t *)b;
    return (x > y) - (x < y);
}

static inline uint64_t bench_percentile(const uint64_t *sorted, int n, int pct)
{
    /* clamp to valid range and use nearest-rank */
    int idx = (int)((long long)n * pct / 100);
    if (idx >= n) idx = n - 1;
    return sorted[idx];
}

/* -----------------------------------------------------------------------
 * Reporting
 * ---------------------------------------------------------------------- */

/*
 * bench_ctx_report — print per-iteration latency statistics.
 *
 * Output columns: n | min | p50 | p95 | p99 | max | mean | stddev (all ns)
 */
static inline void bench_ctx_report(const bench_ctx_t *ctx)
{
    if (ctx->count == 0) {
        printf("  %-52s  (no samples)\n", ctx->name);
        return;
    }

    uint64_t *sorted = (uint64_t *)malloc((size_t)ctx->count * sizeof(uint64_t));
    if (!sorted) { fprintf(stderr, "bench_harness: out of memory\n"); exit(1); }
    memcpy(sorted, ctx->samples, (size_t)ctx->count * sizeof(uint64_t));
    qsort(sorted, (size_t)ctx->count, sizeof(uint64_t), bench_cmp_u64);

    double sum = 0.0;
    for (int i = 0; i < ctx->count; i++) sum += (double)sorted[i];
    double mean = sum / (double)ctx->count;

    double var = 0.0;
    for (int i = 0; i < ctx->count; i++) {
        double d = (double)sorted[i] - mean;
        var += d * d;
    }
    double stddev = ctx->count > 1 ? sqrt(var / (double)(ctx->count - 1)) : 0.0;

    double p50 = (double)bench_percentile(sorted, ctx->count, 50);
    double p95 = (double)bench_percentile(sorted, ctx->count, 95);
    double p99 = (double)bench_percentile(sorted, ctx->count, 99);
    double mn  = (double)sorted[0];
    double mx  = (double)sorted[ctx->count - 1];

    printf("  %-52s  n=%-6d  min=%7.0f ns  p50=%7.0f ns  p95=%7.0f ns"
           "  p99=%7.0f ns  max=%7.0f ns  mean=%7.0f ns  stddev=%6.0f ns\n",
           ctx->name, ctx->count, mn, p50, p95, p99, mx, mean, stddev);

    free(sorted);
}

/*
 * bench_ctx_report_throughput — print per-iteration time AND derived throughput.
 *
 * @units_per_iter: number of "units" (bytes, frames, …) processed each iteration
 * @unit_name:      label for the unit (e.g. "frames", "bytes")
 */
static inline void bench_ctx_report_throughput(const bench_ctx_t *ctx,
                                                size_t             units_per_iter,
                                                const char        *unit_name)
{
    bench_ctx_report(ctx);

    if (ctx->count == 0 || units_per_iter == 0) return;

    /* total wall time = sum of all sample durations */
    double total_ns = 0.0;
    for (int i = 0; i < ctx->count; i++) total_ns += (double)ctx->samples[i];

    double total_units  = (double)ctx->count * (double)units_per_iter;
    double throughput   = total_units / (total_ns / 1e9);  /* units/sec */
    double throughput_M = throughput / 1e6;

    printf("    => throughput: %.2f M%s/s  (%.2f %s/iter, %d iters)\n",
           throughput_M, unit_name,
           (double)units_per_iter, unit_name,
           ctx->count);
}

/* -----------------------------------------------------------------------
 * Convenience section header printer
 * ---------------------------------------------------------------------- */

static inline void bench_section(const char *title)
{
    printf("\n%s\n", title);
    /* underline with dashes */
    for (size_t i = 0; title[i]; i++) putchar('-');
    putchar('\n');
}

#endif /* BENCH_HARNESS_H */
