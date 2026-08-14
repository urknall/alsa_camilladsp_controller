/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * bench/bench_ringbuffer.c — ring buffer microbenchmarks
 *
 * Measures the throughput and per-period overhead of the lock-free
 * pcdsp_ringbuffer_t used between the ALSA transfer callback (producer)
 * and the worker thread (consumer).
 *
 * Benchmarks
 * ----------
 * Single-threaded (write then read, same thread — measures raw API overhead):
 *   rb_write_1024_frames_4b    write 1024 S16-stereo frames (4 B/frame)
 *   rb_read_1024_frames_4b     read 1024 S16-stereo frames
 *   rb_roundtrip_1024_4b       write + read 1024 S16-stereo frames
 *   rb_write_1024_frames_8b    write 1024 S32-stereo frames (8 B/frame)
 *   rb_roundtrip_1024_8b       write + read 1024 S32-stereo frames
 *   rb_roundtrip_64_4b         write + read 64-frame period (low-latency path)
 *   rb_roundtrip_256_4b        write + read 256-frame period
 *   rb_reset_overhead          pcdsp_rb_reset on a full-sized buffer
 *
 * Producer-consumer (two threads — measures real inter-thread path):
 *   rb_pc_throughput_1024_4b   N full periods through a producer + consumer thread
 *
 * All timings are wall-clock nanoseconds measured with CLOCK_MONOTONIC.
 */

#include "bench_harness.h"
#include "ringbuffer.h"

#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* -----------------------------------------------------------------------
 * Constants
 * ---------------------------------------------------------------------- */

#define ITERS_SINGLE   5000   /* single-threaded benchmark iterations       */
#define ITERS_PC        500   /* producer-consumer benchmark iterations     */
#define WARMUP          200   /* warm-up passes (not recorded)              */

/* Ring buffer sized to hold 4 periods without blocking. */
#define RB_CAPACITY   4096u   /* frames (power of two)                      */

/* -----------------------------------------------------------------------
 * Helpers
 * ---------------------------------------------------------------------- */

/* Allocate and fill a sample buffer with a deterministic pattern. */
static uint8_t *make_src_buf(size_t period_frames, size_t frame_bytes)
{
    size_t total = period_frames * frame_bytes;
    uint8_t *buf = (uint8_t *)malloc(total);
    if (!buf) { fprintf(stderr, "malloc failed\n"); exit(1); }
    for (size_t i = 0; i < total; i++)
        buf[i] = (uint8_t)(i & 0xff);
    return buf;
}

static uint8_t *make_dst_buf(size_t period_frames, size_t frame_bytes)
{
    size_t total = period_frames * frame_bytes;
    uint8_t *buf = (uint8_t *)calloc(1, total);
    if (!buf) { fprintf(stderr, "calloc failed\n"); exit(1); }
    return buf;
}

/* -----------------------------------------------------------------------
 * Single-threaded benchmarks
 * ---------------------------------------------------------------------- */

static void bench_write(const char *name, size_t period_frames, size_t frame_bytes)
{
    pcdsp_ringbuffer_t rb;
    pcdsp_rb_init(&rb, RB_CAPACITY, frame_bytes);

    uint8_t *src = make_src_buf(period_frames, frame_bytes);
    bench_ctx_t ctx;
    bench_ctx_init(&ctx, name, ITERS_SINGLE);

    /* warm-up */
    for (int i = 0; i < WARMUP; i++) {
        pcdsp_rb_reset(&rb);
        pcdsp_rb_write(&rb, src, period_frames);
    }

    for (int i = 0; i < ctx.capacity; i++) {
        pcdsp_rb_reset(&rb);
        bench_iter_start(&ctx);
        pcdsp_rb_write(&rb, src, period_frames);
        bench_iter_end(&ctx);
    }

    bench_ctx_report_throughput(&ctx, period_frames, "frames");
    bench_ctx_free(&ctx);
    free(src);
    pcdsp_rb_free(&rb);
}

static void bench_read(const char *name, size_t period_frames, size_t frame_bytes)
{
    pcdsp_ringbuffer_t rb;
    pcdsp_rb_init(&rb, RB_CAPACITY, frame_bytes);

    uint8_t *src = make_src_buf(period_frames, frame_bytes);
    uint8_t *dst = make_dst_buf(period_frames, frame_bytes);
    bench_ctx_t ctx;
    bench_ctx_init(&ctx, name, ITERS_SINGLE);

    /* warm-up */
    for (int i = 0; i < WARMUP; i++) {
        pcdsp_rb_reset(&rb);
        pcdsp_rb_write(&rb, src, period_frames);
        pcdsp_rb_read(&rb, dst, period_frames);
    }

    for (int i = 0; i < ctx.capacity; i++) {
        pcdsp_rb_reset(&rb);
        pcdsp_rb_write(&rb, src, period_frames);
        bench_iter_start(&ctx);
        pcdsp_rb_read(&rb, dst, period_frames);
        bench_iter_end(&ctx);
    }

    bench_ctx_report_throughput(&ctx, period_frames, "frames");
    bench_ctx_free(&ctx);
    free(src);
    free(dst);
    pcdsp_rb_free(&rb);
}

static void bench_roundtrip(const char *name, size_t period_frames, size_t frame_bytes)
{
    pcdsp_ringbuffer_t rb;
    pcdsp_rb_init(&rb, RB_CAPACITY, frame_bytes);

    uint8_t *src = make_src_buf(period_frames, frame_bytes);
    uint8_t *dst = make_dst_buf(period_frames, frame_bytes);
    bench_ctx_t ctx;
    bench_ctx_init(&ctx, name, ITERS_SINGLE);

    /* warm-up */
    for (int i = 0; i < WARMUP; i++) {
        pcdsp_rb_reset(&rb);
        pcdsp_rb_write(&rb, src, period_frames);
        pcdsp_rb_read(&rb, dst, period_frames);
    }

    for (int i = 0; i < ctx.capacity; i++) {
        pcdsp_rb_reset(&rb);
        bench_iter_start(&ctx);
        pcdsp_rb_write(&rb, src, period_frames);
        pcdsp_rb_read(&rb, dst, period_frames);
        bench_iter_end(&ctx);
    }

    bench_ctx_report_throughput(&ctx, period_frames * 2, "frames");
    bench_ctx_free(&ctx);
    free(src);
    free(dst);
    pcdsp_rb_free(&rb);
}

static void bench_reset_overhead(void)
{
    pcdsp_ringbuffer_t rb;
    pcdsp_rb_init(&rb, RB_CAPACITY, 8);
    bench_ctx_t ctx;
    bench_ctx_init(&ctx, "rb_reset_overhead (4096-frame, 8 B/frame)", ITERS_SINGLE);

    for (int i = 0; i < WARMUP; i++) pcdsp_rb_reset(&rb);

    for (int i = 0; i < ctx.capacity; i++) {
        bench_iter_start(&ctx);
        pcdsp_rb_reset(&rb);
        bench_iter_end(&ctx);
    }

    bench_ctx_report(&ctx);
    bench_ctx_free(&ctx);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * Producer-consumer benchmark
 *
 * One real producer thread writes ITERS_PC periods into the ring buffer.
 * The consumer thread (main) reads them back.  We measure the wall-clock
 * time for the entire transfer and derive frames/sec.
 *
 * This exercises the full acquire/release atomic path between two threads.
 * ---------------------------------------------------------------------- */

typedef struct {
    pcdsp_ringbuffer_t *rb;
    const uint8_t      *src;
    size_t              period_frames;
    int                 iters;
    atomic_int          ready;   /* consumer signals "go" */
    atomic_int          done;    /* producer signals "finished" */
} pc_args_t;

static void *producer_thread(void *arg)
{
    pc_args_t *a = (pc_args_t *)arg;

    /* Wait for consumer to be ready. */
    while (!atomic_load_explicit(&a->ready, memory_order_acquire))
        ;

    for (int i = 0; i < a->iters; i++) {
        /* Spin-wait for space (simulates the ALSA transfer callback). */
        while (pcdsp_rb_write_avail(a->rb) < a->period_frames)
            ;
        pcdsp_rb_write(a->rb, a->src, a->period_frames);
    }

    atomic_store_explicit(&a->done, 1, memory_order_release);
    return NULL;
}

static void bench_producer_consumer(void)
{
    const size_t period_frames = 1024;
    const size_t frame_bytes   = 4;   /* S16 stereo */
    const int    iters         = ITERS_PC;

    pcdsp_ringbuffer_t rb;
    pcdsp_rb_init(&rb, RB_CAPACITY, frame_bytes);

    uint8_t *src = make_src_buf(period_frames, frame_bytes);
    uint8_t *dst = make_dst_buf(period_frames, frame_bytes);

    pc_args_t args = {
        .rb            = &rb,
        .src           = src,
        .period_frames = period_frames,
        .iters         = iters,
    };
    atomic_init(&args.ready, 0);
    atomic_init(&args.done,  0);

    /* Warm-up run (not timed). */
    for (int w = 0; w < 2; w++) {
        pcdsp_rb_reset(&rb);
        atomic_store(&args.ready, 0);
        atomic_store(&args.done,  0);
        args.iters = 20;

        pthread_t tid;
        pthread_create(&tid, NULL, producer_thread, &args);
        atomic_store_explicit(&args.ready, 1, memory_order_release);
        for (int i = 0; i < 20; i++) {
            while (pcdsp_rb_read_avail(&rb) < period_frames) ;
            pcdsp_rb_read(&rb, dst, period_frames);
        }
        pthread_join(tid, NULL);
    }

    /* Measured run. */
    pcdsp_rb_reset(&rb);
    atomic_store(&args.ready, 0);
    atomic_store(&args.done,  0);
    args.iters = iters;

    pthread_t tid;
    pthread_create(&tid, NULL, producer_thread, &args);

    uint64_t t_start = bench_clock_ns();
    atomic_store_explicit(&args.ready, 1, memory_order_release);

    for (int i = 0; i < iters; i++) {
        while (pcdsp_rb_read_avail(&rb) < period_frames) ;
        pcdsp_rb_read(&rb, dst, period_frames);
    }

    uint64_t t_end = bench_clock_ns();
    pthread_join(tid, NULL);

    double elapsed_s  = (double)(t_end - t_start) / 1e9;
    double total_frames = (double)iters * (double)period_frames;
    double fps          = total_frames / elapsed_s;

    printf("  %-52s  iters=%-4d  period=%-5zu frames  total=%.0f frames"
           "  elapsed=%.3f ms  throughput=%.2f Mframes/s\n",
           "rb_producer_consumer_throughput (1024 frames, 4 B/frame)",
           iters, period_frames, total_frames,
           elapsed_s * 1e3, fps / 1e6);

    free(src);
    free(dst);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("bench_ringbuffer\n");

    bench_section("Write (single-threaded)");
    bench_write("rb_write_1024_frames_4b (S16 stereo)",   1024, 4);
    bench_write("rb_write_1024_frames_8b (S32 stereo)",   1024, 8);
    bench_write("rb_write_64_frames_4b (small period)",     64, 4);
    bench_write("rb_write_256_frames_4b",                  256, 4);

    bench_section("Read (single-threaded)");
    bench_read("rb_read_1024_frames_4b (S16 stereo)",     1024, 4);
    bench_read("rb_read_1024_frames_8b (S32 stereo)",     1024, 8);

    bench_section("Round-trip write+read (single-threaded)");
    bench_roundtrip("rb_roundtrip_1024_4b (S16 stereo)",  1024, 4);
    bench_roundtrip("rb_roundtrip_1024_8b (S32 stereo)",  1024, 8);
    bench_roundtrip("rb_roundtrip_256_4b",                 256, 4);
    bench_roundtrip("rb_roundtrip_64_4b (low-latency)",     64, 4);

    bench_section("Misc");
    bench_reset_overhead();

    bench_section("Producer-consumer (two threads)");
    bench_producer_consumer();

    printf("\nbench_ringbuffer done\n");
    return 0;
}
