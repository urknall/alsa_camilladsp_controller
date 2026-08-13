/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * format.h — PCM format utilities
 */

#ifndef PICOREDSP_FORMAT_H
#define PICOREDSP_FORMAT_H

#include <alsa/asoundlib.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * pcdsp_format_phys_bytes — bytes per sample in memory for the given format.
 * Returns 0 and sets *out on success; returns -EINVAL for unsupported formats.
 */
int pcdsp_format_phys_bytes(snd_pcm_format_t fmt, size_t *out);

/*
 * pcdsp_format_frame_bytes — bytes per frame (all channels) for the given
 * format and channel count.
 */
int pcdsp_format_frame_bytes(snd_pcm_format_t fmt, unsigned int channels, size_t *out);

/*
 * pcdsp_format_supported — returns 1 if the format is supported, 0 otherwise.
 */
int pcdsp_format_supported(snd_pcm_format_t fmt);

/*
 * pcdsp_format_list — fill `list` with supported snd_pcm_format_t values cast
 * to unsigned int (for use with snd_pcm_ioplug_set_param_list).
 * Returns the number of entries written.
 */
size_t pcdsp_format_list(unsigned int *list, size_t max_count);

#ifdef __cplusplus
}
#endif

#endif /* PICOREDSP_FORMAT_H */
