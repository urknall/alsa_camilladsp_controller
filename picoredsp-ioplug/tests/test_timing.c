/*
 * tests/test_timing.c — unit tests for pcdsp stream timing helpers
 *
 * Tests cover:
 *   - pcdsp_clock_now returns a non-zero value
 *   - pcdsp_clock_now is monotonically non-decreasing across two calls
 *   - timer starts and stops correctly
 *   - elapsed_frames returns 0 when not running
 *   - elapsed_frames grows after a real sleep
 *   - elapsed_frames returns 0 after stop
 */

#include "timing.h"

#include <assert.h>
#include <stdio.h>
#include <time.h>

static int g_pass = 0;
static int g_fail = 0;

#define CHECK(expr) \
    do { \
        if (!(expr)) { \
            printf("FAIL\n  assertion failed: %s  (%s:%d)\n", #expr, __FILE__, __LINE__); \
            g_fail++; \
            return; \
        } \
    } while (0)

#define TEST(name) static void test_##name(void)
#define RUN(name)  do { printf("  %s ... ", #name); test_##name(); printf("ok\n"); g_pass++; } while (0)

static void sleep_ms(long ms)
{
    struct timespec ts = { .tv_sec = ms / 1000, .tv_nsec = (ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

/* -----------------------------------------------------------------------
 * Tests
 * ---------------------------------------------------------------------- */

TEST(clock_now_nonzero)
{
    uint64_t t;
    CHECK(pcdsp_clock_now(&t) == 0);
    CHECK(t > 0);
}

TEST(clock_now_monotonic)
{
    uint64_t t1, t2;
    CHECK(pcdsp_clock_now(&t1) == 0);
    sleep_ms(5);
    CHECK(pcdsp_clock_now(&t2) == 0);
    CHECK(t2 >= t1);
}

TEST(timer_elapsed_zero_when_not_started)
{
    pcdsp_stream_timer_t t;
    pcdsp_timer_init(&t, 48000);
    CHECK(pcdsp_timer_elapsed_frames(&t) == 0);
}

TEST(timer_elapsed_grows_after_start)
{
    pcdsp_stream_timer_t t;
    pcdsp_timer_init(&t, 48000);
    pcdsp_timer_start(&t);
    sleep_ms(50);  /* ~2400 frames at 48 kHz */
    uint64_t frames = pcdsp_timer_elapsed_frames(&t);
    /* Generous tolerance: at least 1000 frames, at most 5000 */
    CHECK(frames >= 1000);
    CHECK(frames <= 5000);
    pcdsp_timer_stop(&t);
}

TEST(timer_elapsed_zero_after_stop)
{
    pcdsp_stream_timer_t t;
    pcdsp_timer_init(&t, 48000);
    pcdsp_timer_start(&t);
    sleep_ms(10);
    pcdsp_timer_stop(&t);
    CHECK(pcdsp_timer_elapsed_frames(&t) == 0);
}

TEST(timer_restart_resets_origin)
{
    pcdsp_stream_timer_t t;
    pcdsp_timer_init(&t, 44100);
    pcdsp_timer_start(&t);
    sleep_ms(20);
    uint64_t f1 = pcdsp_timer_elapsed_frames(&t);
    pcdsp_timer_stop(&t);

    /* Restart from zero. */
    pcdsp_timer_start(&t);
    sleep_ms(5);
    uint64_t f2 = pcdsp_timer_elapsed_frames(&t);
    pcdsp_timer_stop(&t);

    /* f2 should be less than f1 since we slept shorter. */
    CHECK(f2 < f1);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("test_timing\n");

    RUN(clock_now_nonzero);
    RUN(clock_now_monotonic);
    RUN(timer_elapsed_zero_when_not_started);
    RUN(timer_elapsed_grows_after_start);
    RUN(timer_elapsed_zero_after_stop);
    RUN(timer_restart_resets_origin);

    printf("\n%d passed, %d failed\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
}
