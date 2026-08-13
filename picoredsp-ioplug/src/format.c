/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * format.c — PCM format utilities
 *
 * Maps between ALSA snd_pcm_format_t values and the byte widths / frame
 * sizes used internally.  Only formats that piCoreDSP intends to support
 * are listed; unknown formats are rejected at hw_params time.
 */

#include "format.h"

#include <alsa/asoundlib.h>
#include <errno.h>

/* Supported formats: name, sample_bytes, physical_bytes */
static const struct {
    snd_pcm_format_t fmt;
    size_t           sample_bytes;   /* significant bytes per sample */
    size_t           phys_bytes;     /* bytes occupied in memory     */
} format_table[] = {
    { SND_PCM_FORMAT_S16_LE,   2, 2 },
    { SND_PCM_FORMAT_S24_3LE,  3, 3 },
    { SND_PCM_FORMAT_S24_LE,   3, 4 },
    { SND_PCM_FORMAT_S32_LE,   4, 4 },
    { SND_PCM_FORMAT_FLOAT_LE, 4, 4 },
};

#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))

int pcdsp_format_phys_bytes(snd_pcm_format_t fmt, size_t *out)
{
    for (size_t i = 0; i < ARRAY_SIZE(format_table); i++) {
        if (format_table[i].fmt == fmt) {
            if (out)
                *out = format_table[i].phys_bytes;
            return 0;
        }
    }
    return -EINVAL;
}

int pcdsp_format_frame_bytes(snd_pcm_format_t fmt, unsigned int channels, size_t *out)
{
    size_t phys;
    int rc = pcdsp_format_phys_bytes(fmt, &phys);
    if (rc < 0)
        return rc;
    if (out)
        *out = phys * channels;
    return 0;
}

int pcdsp_format_supported(snd_pcm_format_t fmt)
{
    return pcdsp_format_phys_bytes(fmt, NULL) == 0 ? 1 : 0;
}

/*
 * pcdsp_format_list — fill `list` with supported snd_pcm_format_t values.
 * Returns the number of entries written (≤ max_count).
 */
size_t pcdsp_format_list(unsigned int *list, size_t max_count)
{
    size_t n = ARRAY_SIZE(format_table);
    if (n > max_count)
        n = max_count;
    for (size_t i = 0; i < n; i++)
        list[i] = (unsigned int)format_table[i].fmt;
    return n;
}
