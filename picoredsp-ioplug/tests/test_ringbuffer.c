/*
 * tests/test_ringbuffer.c — unit tests for pcdsp_ringbuffer_t
 *
 * Tests cover:
 *   - initialisation / free
 *   - write_avail / read_avail accounting
 *   - write fills buffer exactly up to capacity
 *   - read recovers written data correctly
 *   - wrap-around (multiple periods across the ring boundary)
 *   - drop discards frames and releases write space
 *   - reset restores a full buffer back to empty
 *   - partial write (producer cannot overfill)
 *   - partial read (consumer cannot underread)
 *   - init rejects non-power-of-two capacity
 *   - init rejects capacity < 2
 */

#include "ringbuffer.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* -----------------------------------------------------------------------
 * Micro test framework
 * ---------------------------------------------------------------------- */

static int g_pass = 0;
static int g_fail = 0;

#define TEST(name) static void test_##name(void)
#define RUN(name)  do { printf("  %s ... ", #name); test_##name(); printf("ok\n"); g_pass++; } while (0)

#define CHECK(expr) \
    do { \
        if (!(expr)) { \
            printf("FAIL\n  assertion failed: %s  (%s:%d)\n", #expr, __FILE__, __LINE__); \
            g_fail++; \
            return; \
        } \
    } while (0)

/* -----------------------------------------------------------------------
 * Tests
 * ---------------------------------------------------------------------- */

TEST(init_free)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, 4) == 0);
    CHECK(rb.capacity    == 16);
    CHECK(rb.mask        == 15);
    CHECK(rb.frame_bytes == 4);
    pcdsp_rb_free(&rb);
    CHECK(rb.buf == NULL);
}

TEST(init_rejects_non_power_of_two)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 15, 4) == -22 /* EINVAL */);
    CHECK(pcdsp_rb_init(&rb,  3, 4) == -22);
    CHECK(pcdsp_rb_init(&rb,  0, 4) == -22);
    CHECK(pcdsp_rb_init(&rb,  1, 4) == -22); /* < 2 */
}

TEST(init_rejects_zero_frame_bytes)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, 0) == -22);
}

TEST(empty_buffer_has_zero_read_avail)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, 4) == 0);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);
    CHECK(pcdsp_rb_write_avail(&rb) == 16);
    pcdsp_rb_free(&rb);
}

TEST(write_read_single_period)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 16, 4) == 0);

    uint8_t src[16 * 4];
    for (int i = 0; i < (int)sizeof(src); i++)
        src[i] = (uint8_t)i;

    size_t written = pcdsp_rb_write(&rb, src, 8);
    CHECK(written == 8);
    CHECK(pcdsp_rb_read_avail(&rb) == 8);
    CHECK(pcdsp_rb_write_avail(&rb) == 8);

    uint8_t dst[8 * 4];
    size_t read = pcdsp_rb_read(&rb, dst, 8);
    CHECK(read == 8);
    CHECK(memcmp(src, dst, sizeof(dst)) == 0);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);
    CHECK(pcdsp_rb_write_avail(&rb) == 16);

    pcdsp_rb_free(&rb);
}

TEST(write_fills_to_capacity)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, 2) == 0);

    uint8_t src[8 * 2];
    memset(src, 0xAB, sizeof(src));

    size_t w = pcdsp_rb_write(&rb, src, 8);
    CHECK(w == 8);
    CHECK(pcdsp_rb_write_avail(&rb) == 0);

    /* Trying to write one more should produce 0. */
    size_t w2 = pcdsp_rb_write(&rb, src, 1);
    CHECK(w2 == 0);

    pcdsp_rb_free(&rb);
}

TEST(wrap_around)
{
    /* Fill half, read half, fill half again — forces wrap at end of buffer. */
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, 4) == 0);

    uint8_t src[4 * 4];
    uint8_t dst[4 * 4];

    for (int i = 0; i < (int)sizeof(src); i++)
        src[i] = (uint8_t)(i + 1);

    /* Write 4, read 4 (write_pos = 4, read_pos = 4) */
    CHECK(pcdsp_rb_write(&rb, src, 4) == 4);
    CHECK(pcdsp_rb_read(&rb, dst, 4) == 4);
    CHECK(memcmp(src, dst, sizeof(dst)) == 0);

    /* Write 6 — last 2 will wrap around the end of the buffer */
    uint8_t src2[6 * 4];
    for (int i = 0; i < (int)sizeof(src2); i++)
        src2[i] = (uint8_t)(0x80 + i);

    CHECK(pcdsp_rb_write(&rb, src2, 6) == 6);
    CHECK(pcdsp_rb_read_avail(&rb) == 6);

    uint8_t dst2[6 * 4];
    CHECK(pcdsp_rb_read(&rb, dst2, 6) == 6);
    CHECK(memcmp(src2, dst2, sizeof(dst2)) == 0);

    pcdsp_rb_free(&rb);
}

TEST(drop_advances_read_pos)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, 4) == 0);

    uint8_t src[8 * 4];
    memset(src, 0x55, sizeof(src));
    CHECK(pcdsp_rb_write(&rb, src, 8) == 8);

    size_t dropped = pcdsp_rb_drop(&rb, 4);
    CHECK(dropped == 4);
    CHECK(pcdsp_rb_read_avail(&rb) == 4);
    CHECK(pcdsp_rb_write_avail(&rb) == 4);

    /* Drop more than available — clamped */
    size_t dropped2 = pcdsp_rb_drop(&rb, 10);
    CHECK(dropped2 == 4);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);

    pcdsp_rb_free(&rb);
}

TEST(reset_empties_buffer)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, 4) == 0);

    uint8_t src[8 * 4];
    memset(src, 0, sizeof(src));
    CHECK(pcdsp_rb_write(&rb, src, 8) == 8);
    CHECK(pcdsp_rb_read_avail(&rb) == 8);

    pcdsp_rb_reset(&rb);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);
    CHECK(pcdsp_rb_write_avail(&rb) == 8);

    pcdsp_rb_free(&rb);
}

TEST(partial_write_at_boundary)
{
    /* Write 5 into a capacity-8 buffer that already has 5 frames — should
     * only write 3. */
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, 2) == 0);

    uint8_t src[8 * 2];
    memset(src, 0x7E, sizeof(src));

    CHECK(pcdsp_rb_write(&rb, src, 5) == 5);
    size_t w = pcdsp_rb_write(&rb, src, 5);
    CHECK(w == 3);
    CHECK(pcdsp_rb_write_avail(&rb) == 0);

    pcdsp_rb_free(&rb);
}

TEST(partial_read_at_boundary)
{
    pcdsp_ringbuffer_t rb;
    CHECK(pcdsp_rb_init(&rb, 8, 2) == 0);

    uint8_t src[3 * 2];
    memset(src, 0x3C, sizeof(src));
    CHECK(pcdsp_rb_write(&rb, src, 3) == 3);

    uint8_t dst[8 * 2];
    /* Asking for 8 but only 3 available */
    size_t r = pcdsp_rb_read(&rb, dst, 8);
    CHECK(r == 3);
    CHECK(pcdsp_rb_read_avail(&rb) == 0);

    pcdsp_rb_free(&rb);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("test_ringbuffer\n");

    RUN(init_free);
    RUN(init_rejects_non_power_of_two);
    RUN(init_rejects_zero_frame_bytes);
    RUN(empty_buffer_has_zero_read_avail);
    RUN(write_read_single_period);
    RUN(write_fills_to_capacity);
    RUN(wrap_around);
    RUN(drop_advances_read_pos);
    RUN(reset_empties_buffer);
    RUN(partial_write_at_boundary);
    RUN(partial_read_at_boundary);

    printf("\n%d passed, %d failed\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
}
