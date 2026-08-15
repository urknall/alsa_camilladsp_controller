/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * ringbuffer.h — lock-free single-producer / single-consumer ring buffer
 *
 * The ring buffer is shared between the ALSA ioplug transfer callback
 * (producer) and the worker thread that drains samples to the pipe /
 * null sink (consumer).  All synchronisation is performed with C11
 * atomic load/store using acquire/release ordering — no locks, no
 * condition variables on the data path.
 *
 * Terminology used throughout:
 *   write_pos  — next frame slot the producer will write into
 *   read_pos   — next frame slot the consumer will read from
 *   capacity   — total number of frame slots allocated
 *
 * Invariants:
 *   0 ≤ read_pos ≤ write_pos          (monotonically increasing)
 *   write_pos - read_pos ≤ capacity   (never overfull)
 *
 * Positions are never wrapped; masking (pos & mask) converts them to
 * array indices.  This avoids the ABA problems that arise when the
 * counters wrap at capacity.  The counters are 64-bit so overflow is
 * not a practical concern.
 */

#ifndef PICOREDSP_RINGBUFFER_H
#define PICOREDSP_RINGBUFFER_H

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct pcdsp_ringbuffer {
    uint8_t        *buf;          /* raw sample bytes, capacity * frame_bytes */
    size_t          capacity;     /* frame slots; MUST be a power of two      */
    size_t          mask;         /* capacity - 1                              */
    size_t          frame_bytes;  /* bytes per frame = channels * sample_bytes */

    /* Producer writes write_pos; consumer reads it (acquire). */
    _Atomic(uint64_t) write_pos;
    /* Consumer writes read_pos; producer reads it (acquire).  */
    _Atomic(uint64_t) read_pos;
} pcdsp_ringbuffer_t;

/*
 * pcdsp_rb_init — allocate and initialise a ring buffer.
 *
 * capacity_frames must be a power of two and ≥ 2.
 * Returns 0 on success, -EINVAL / -ENOMEM on failure.
 */
int pcdsp_rb_init(pcdsp_ringbuffer_t *rb,
                  size_t capacity_frames,
                  size_t frame_bytes);

/* pcdsp_rb_free — release memory allocated by pcdsp_rb_init. */
void pcdsp_rb_free(pcdsp_ringbuffer_t *rb);

/* pcdsp_rb_reset — reset positions to zero (must be called only when
 * neither producer nor consumer is running). */
void pcdsp_rb_reset(pcdsp_ringbuffer_t *rb);

/*
 * pcdsp_rb_write_avail — frames the producer can write without blocking.
 * Reads read_pos with acquire ordering.
 */
size_t pcdsp_rb_write_avail(const pcdsp_ringbuffer_t *rb);

/*
 * pcdsp_rb_read_avail — frames available for the consumer to read.
 * Reads write_pos with acquire ordering.
 */
size_t pcdsp_rb_read_avail(const pcdsp_ringbuffer_t *rb);

/*
 * pcdsp_rb_write — copy up to `frames` frames from `src` into the ring buffer.
 *
 * Returns the number of frames actually written (may be less than `frames`
 * if the buffer is nearly full).  Uses release store on write_pos.
 */
size_t pcdsp_rb_write(pcdsp_ringbuffer_t *rb, const void *src, size_t frames);

/*
 * pcdsp_rb_read — copy up to `frames` frames from the ring buffer into `dst`.
 *
 * Returns the number of frames actually read.  Uses release store on read_pos.
 */
size_t pcdsp_rb_read(pcdsp_ringbuffer_t *rb, void *dst, size_t frames);

/*
 * pcdsp_rb_peek — copy up to `frames` frames from the ring buffer into `dst`
 * WITHOUT advancing read_pos.
 *
 * Use this together with pcdsp_rb_drop() to implement "commit only after a
 * successful downstream handoff" consumers: peek the frames, hand them to a
 * potentially-blocking/cancellable sink (e.g. a pipe write), and only call
 * pcdsp_rb_drop() once the handoff has actually completed. This keeps the
 * ring buffer the single authoritative queue for "has this audio left the
 * plugin yet?" — unlike pcdsp_rb_read(), which removes frames from the ring
 * before the caller has done anything with them, creating a window where the
 * frames are in neither the ring buffer nor the downstream sink.
 *
 * Returns the number of frames actually peeked (may be less than `frames`
 * if fewer are available). Does not mutate read_pos or write_pos.
 */
size_t pcdsp_rb_peek(const pcdsp_ringbuffer_t *rb, void *dst, size_t frames);

/*
 * pcdsp_rb_drop — advance read_pos by `frames` without copying data.
 * Equivalent to discarding `frames` frames (or, paired with pcdsp_rb_peek(),
 * committing frames previously peeked once their downstream handoff has
 * completed).
 */
size_t pcdsp_rb_drop(pcdsp_ringbuffer_t *rb, size_t frames);

#ifdef __cplusplus
}
#endif

#endif /* PICOREDSP_RINGBUFFER_H */
