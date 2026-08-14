/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * bench/bench_pcm_worker.c — PCM worker helper microbenchmarks
 *
 * Measures the throughput and per-call overhead of the two worker-thread
 * helpers:
 *
 *   pcdsp_drain_period_null_sink  — drops frames from the ring buffer with
 *                                   an optional nominal-rate sleep.  Tested
 *                                   here with rate=0 (no sleep) to measure
 *                                   raw drop throughput.
 *
 *   pcdsp_drain_period_to_pipe    — drains one period from the ring buffer
 *                                   and writes it to a pipe fd.  Tested with
 *                                   a reader thread consuming the pipe to
 *                                   avoid blocking.
 *
 * Benchmarks
 * ----------
 *   null_sink_drain_64_4b         drain 64-frame period, 4 B/frame, rate=0
 *   null_sink_drain_256_4b        drain 256-frame period, 4 B/frame, rate=0
 *   null_sink_drain_1024_4b       drain 1024-frame period, 4 B/frame, rate=0
 *   null_sink_drain_1024_8b       drain 1024-frame period, 8 B/frame, rate=0
 *   pipe_drain_1024_4b            drain 1024-frame period to a real pipe fd
 *   pipe_drain_1024_8b            drain 1024-frame period to a real pipe fd
 */

#include "bench_harness.h"
#include "pcm_worker.h"
#include "ringbuffer.h"

#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define ITERS_NULL_SINK  5000
#define ITERS_PIPE        500
#define WARMUP            200

/* -----------------------------------------------------------------------
 * Helpers
 * ---------------------------------------------------------------------- */

static uint8_t *make_src_buf(size_t frames, size_t frame_bytes)
{
    size_t n = frames * frame_bytes;
    uint8_t *buf = (uint8_t *)malloc(n);
    if (!buf) { perror("malloc"); exit(1); }
    for (size_t i = 0; i < n; i++) buf[i] = (uint8_t)(i & 0xff);
    return buf;
}

/* Fill rb with one period of src data (reset first). */
static void fill_rb(pcdsp_ringbuffer_t *rb, const uint8_t *src, size_t period_frames)
{
    pcdsp_rb_reset(rb);
    pcdsp_rb_write(rb, src, period_frames);
}

/* -----------------------------------------------------------------------
 * Null-sink drain benchmarks
 * ---------------------------------------------------------------------- */

static void bench_null_sink(const char *name, size_t period_frames, size_t frame_bytes)
{
    pcdsp_ringbuffer_t rb;
    /* capacity must exceed period_frames for wrap-around tests */
    size_t cap = 1;
    while (cap < period_frames * 2) cap <<= 1;
    pcdsp_rb_init(&rb, cap, frame_bytes);

    uint8_t *src = make_src_buf(period_frames, frame_bytes);
    bench_ctx_t ctx;
    bench_ctx_init(&ctx, name, ITERS_NULL_SINK);

    /* warm-up */
    for (int i = 0; i < WARMUP; i++) {
        fill_rb(&rb, src, period_frames);
        pcdsp_drain_period_null_sink(&rb, period_frames, 0 /* no sleep */);
    }

    for (int i = 0; i < ctx.capacity; i++) {
        fill_rb(&rb, src, period_frames);
        bench_iter_start(&ctx);
        pcdsp_drain_period_null_sink(&rb, period_frames, 0);
        bench_iter_end(&ctx);
    }

    bench_ctx_report_throughput(&ctx, period_frames, "frames");
    bench_ctx_free(&ctx);
    free(src);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * Pipe-drain benchmark
 *
 * A reader thread reads all data from the pipe so the producer never blocks.
 * ---------------------------------------------------------------------- */

typedef struct {
    int      pipe_rd;      /* read end of the pipe fd */
    size_t   bytes_total;  /* total bytes to read before exiting */
    atomic_int ready;      /* set to 1 by main when it is about to start */
} pipe_reader_args_t;

static void *pipe_reader_thread(void *arg)
{
    pipe_reader_args_t *a = (pipe_reader_args_t *)arg;

    /* Wait for the benchmark loop to start. */
    while (!atomic_load_explicit(&a->ready, memory_order_acquire))
        ;

    uint8_t discard[4096];
    size_t remaining = a->bytes_total;
    while (remaining > 0) {
        size_t want = remaining < sizeof(discard) ? remaining : sizeof(discard);
        ssize_t n = read(a->pipe_rd, discard, want);
        if (n <= 0) break;
        remaining -= (size_t)n;
    }
    return NULL;
}

static void bench_pipe_drain(const char *name, size_t period_frames, size_t frame_bytes)
{
    pcdsp_ringbuffer_t rb;
    size_t cap = 1;
    while (cap < period_frames * 2) cap <<= 1;
    pcdsp_rb_init(&rb, cap, frame_bytes);

    uint8_t *src = make_src_buf(period_frames, frame_bytes);

    /* Create a real pipe. */
    int fds[2];
    if (pipe(fds) != 0) { perror("pipe"); exit(1); }
    int pipe_rd = fds[0];
    int pipe_wr = fds[1];

    bench_ctx_t ctx;
    bench_ctx_init(&ctx, name, ITERS_PIPE);

    /*
     * Warmup: write one period then immediately drain it back (non-blocking
     * reads) so the pipe never accumulates more than one period and never
     * blocks regardless of frame size.
     */
    const int warmup_pipe = 10;
    uint8_t drain_buf[8192];
    int rd_flags = fcntl(pipe_rd, F_GETFL, 0);
    fcntl(pipe_rd, F_SETFL, rd_flags | O_NONBLOCK);

    for (int i = 0; i < warmup_pipe; i++) {
        fill_rb(&rb, src, period_frames);
        pcdsp_drain_period_to_pipe(&rb, pipe_wr, period_frames, frame_bytes, NULL);
        /* drain the period we just wrote (non-blocking) */
        ssize_t remaining = (ssize_t)(period_frames * frame_bytes);
        while (remaining > 0) {
            ssize_t n = read(pipe_rd, drain_buf, sizeof(drain_buf));
            if (n > 0) remaining -= n;
            else break;
        }
    }

    /* Restore blocking mode for the measured phase. */
    fcntl(pipe_rd, F_SETFL, rd_flags);

    /* Start the reader thread to consume all measured output. */
    size_t total_bytes = (size_t)ITERS_PIPE * period_frames * frame_bytes;
    pipe_reader_args_t reader_args = {
        .pipe_rd    = pipe_rd,
        .bytes_total = total_bytes,
    };
    atomic_init(&reader_args.ready, 0);

    pthread_t tid;
    pthread_create(&tid, NULL, pipe_reader_thread, &reader_args);

    atomic_store_explicit(&reader_args.ready, 1, memory_order_release);

    for (int i = 0; i < ctx.capacity; i++) {
        fill_rb(&rb, src, period_frames);
        bench_iter_start(&ctx);
        pcdsp_drain_period_to_pipe(&rb, pipe_wr, period_frames, frame_bytes, NULL);
        bench_iter_end(&ctx);
    }

    pthread_join(tid, NULL);
    close(pipe_rd);
    close(pipe_wr);

    bench_ctx_report_throughput(&ctx, period_frames, "frames");
    bench_ctx_free(&ctx);
    free(src);
    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("bench_pcm_worker\n");

    bench_section("Null-sink drain (rate=0, no sleep)");
    bench_null_sink("null_sink_drain_64f_4b  (S16 stereo)",   64, 4);
    bench_null_sink("null_sink_drain_256f_4b",                256, 4);
    bench_null_sink("null_sink_drain_1024f_4b (S16 stereo)", 1024, 4);
    bench_null_sink("null_sink_drain_1024f_8b (S32 stereo)", 1024, 8);

    bench_section("Pipe drain (real pipe fd, reader thread)");
    bench_pipe_drain("pipe_drain_1024f_4b (S16 stereo)", 1024, 4);
    bench_pipe_drain("pipe_drain_1024f_8b (S32 stereo)", 1024, 8);

    printf("\nbench_pcm_worker done\n");
    return 0;
}
