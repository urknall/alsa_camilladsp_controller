/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * timing.h — stream timing helpers
 */

#ifndef PICOREDSP_TIMING_H
#define PICOREDSP_TIMING_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* pcdsp_clock_now — store current CLOCK_MONOTONIC value in nanoseconds. */
int pcdsp_clock_now(uint64_t *ns_out);

typedef struct {
    uint64_t     start_ns;
    unsigned int rate;
    int          running;
} pcdsp_stream_timer_t;

void     pcdsp_timer_init(pcdsp_stream_timer_t *t, unsigned int rate);
void     pcdsp_timer_start(pcdsp_stream_timer_t *t);
void     pcdsp_timer_stop(pcdsp_stream_timer_t *t);
uint64_t pcdsp_timer_elapsed_frames(const pcdsp_stream_timer_t *t);

#ifdef __cplusplus
}
#endif

#endif /* PICOREDSP_TIMING_H */
