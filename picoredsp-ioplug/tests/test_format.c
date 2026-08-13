/*
 * tests/test_format.c — unit tests for pcdsp format helpers
 *
 * Tests cover:
 *   - known formats return correct physical byte widths
 *   - unknown formats return -EINVAL
 *   - frame byte computation (format × channels)
 *   - pcdsp_format_supported returns 1 for known, 0 for unknown
 *   - pcdsp_format_list returns all known formats
 */

#include "format.h"

#include <alsa/asoundlib.h>
#include <assert.h>
#include <stdio.h>
#include <string.h>

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

/* -----------------------------------------------------------------------
 * Tests
 * ---------------------------------------------------------------------- */

TEST(s16_le_phys_bytes)
{
    size_t b;
    CHECK(pcdsp_format_phys_bytes(SND_PCM_FORMAT_S16_LE, &b) == 0);
    CHECK(b == 2);
}

TEST(s24_3le_phys_bytes)
{
    size_t b;
    CHECK(pcdsp_format_phys_bytes(SND_PCM_FORMAT_S24_3LE, &b) == 0);
    CHECK(b == 3);
}

TEST(s24_le_phys_bytes)
{
    size_t b;
    CHECK(pcdsp_format_phys_bytes(SND_PCM_FORMAT_S24_LE, &b) == 0);
    CHECK(b == 4);  /* 3 significant bytes in 4-byte container */
}

TEST(s32_le_phys_bytes)
{
    size_t b;
    CHECK(pcdsp_format_phys_bytes(SND_PCM_FORMAT_S32_LE, &b) == 0);
    CHECK(b == 4);
}

TEST(float_le_phys_bytes)
{
    size_t b;
    CHECK(pcdsp_format_phys_bytes(SND_PCM_FORMAT_FLOAT_LE, &b) == 0);
    CHECK(b == 4);
}

TEST(unknown_format_einval)
{
    size_t b;
    /* SND_PCM_FORMAT_MU_LAW is not in our support list */
    CHECK(pcdsp_format_phys_bytes(SND_PCM_FORMAT_MU_LAW, &b) == -22 /* EINVAL */);
}

TEST(frame_bytes_stereo_s16)
{
    size_t fb;
    CHECK(pcdsp_format_frame_bytes(SND_PCM_FORMAT_S16_LE, 2, &fb) == 0);
    CHECK(fb == 4);
}

TEST(frame_bytes_8ch_s32)
{
    size_t fb;
    CHECK(pcdsp_format_frame_bytes(SND_PCM_FORMAT_S32_LE, 8, &fb) == 0);
    CHECK(fb == 32);
}

TEST(frame_bytes_unknown_einval)
{
    size_t fb;
    CHECK(pcdsp_format_frame_bytes(SND_PCM_FORMAT_MU_LAW, 2, &fb) == -22);
}

TEST(supported_returns_1_for_known)
{
    CHECK(pcdsp_format_supported(SND_PCM_FORMAT_S16_LE)   == 1);
    CHECK(pcdsp_format_supported(SND_PCM_FORMAT_S24_3LE)  == 1);
    CHECK(pcdsp_format_supported(SND_PCM_FORMAT_S24_LE)   == 1);
    CHECK(pcdsp_format_supported(SND_PCM_FORMAT_S32_LE)   == 1);
    CHECK(pcdsp_format_supported(SND_PCM_FORMAT_FLOAT_LE) == 1);
}

TEST(supported_returns_0_for_unknown)
{
    CHECK(pcdsp_format_supported(SND_PCM_FORMAT_MU_LAW) == 0);
    CHECK(pcdsp_format_supported(SND_PCM_FORMAT_A_LAW)  == 0);
    CHECK(pcdsp_format_supported((snd_pcm_format_t)999) == 0);
}

TEST(format_list_returns_all_known)
{
    unsigned int list[16];
    size_t n = pcdsp_format_list(list, 16);
    CHECK(n >= 5);  /* at least our 5 known formats */

    int found_s16  = 0, found_s32 = 0;
    for (size_t i = 0; i < n; i++) {
        if (list[i] == (unsigned int)SND_PCM_FORMAT_S16_LE) found_s16 = 1;
        if (list[i] == (unsigned int)SND_PCM_FORMAT_S32_LE) found_s32 = 1;
    }
    CHECK(found_s16);
    CHECK(found_s32);
}

TEST(format_list_respects_max_count)
{
    unsigned int list[2];
    size_t n = pcdsp_format_list(list, 2);
    CHECK(n == 2);
}

/* -----------------------------------------------------------------------
 * main
 * ---------------------------------------------------------------------- */

int main(void)
{
    printf("test_format\n");

    RUN(s16_le_phys_bytes);
    RUN(s24_3le_phys_bytes);
    RUN(s24_le_phys_bytes);
    RUN(s32_le_phys_bytes);
    RUN(float_le_phys_bytes);
    RUN(unknown_format_einval);
    RUN(frame_bytes_stereo_s16);
    RUN(frame_bytes_8ch_s32);
    RUN(frame_bytes_unknown_einval);
    RUN(supported_returns_1_for_known);
    RUN(supported_returns_0_for_unknown);
    RUN(format_list_returns_all_known);
    RUN(format_list_respects_max_count);

    printf("\n%d passed, %d failed\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
}
