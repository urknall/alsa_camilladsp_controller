/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * ringbuffer.c — lock-free single-producer / single-consumer ring buffer
 *
 * See ringbuffer.h for design notes and invariants.
 */

#include "ringbuffer.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>

/* -----------------------------------------------------------------------
 * Internal helpers
 * ---------------------------------------------------------------------- */

static inline uint64_t rb_write_pos(const pcdsp_ringbuffer_t *rb)
{
    return atomic_load_explicit(&rb->write_pos, memory_order_acquire);
}

static inline uint64_t rb_read_pos(const pcdsp_ringbuffer_t *rb)
{
    return atomic_load_explicit(&rb->read_pos, memory_order_acquire);
}

/* -----------------------------------------------------------------------
 * Public API
 * ---------------------------------------------------------------------- */

int pcdsp_rb_init(pcdsp_ringbuffer_t *rb,
                  size_t              capacity_frames,
                  size_t              frame_bytes)
{
    if (!rb || capacity_frames < 2 || frame_bytes == 0)
        return -EINVAL;

    /* capacity must be a power of two */
    if (capacity_frames & (capacity_frames - 1))
        return -EINVAL;

    rb->buf = malloc(capacity_frames * frame_bytes);
    if (!rb->buf)
        return -ENOMEM;

    rb->capacity    = capacity_frames;
    rb->mask        = capacity_frames - 1;
    rb->frame_bytes = frame_bytes;

    atomic_init(&rb->write_pos, 0);
    atomic_init(&rb->read_pos,  0);

    return 0;
}

void pcdsp_rb_free(pcdsp_ringbuffer_t *rb)
{
    if (!rb)
        return;
    free(rb->buf);
    rb->buf      = NULL;
    rb->capacity = 0;
    rb->mask     = 0;
}

void pcdsp_rb_reset(pcdsp_ringbuffer_t *rb)
{
    atomic_store_explicit(&rb->write_pos, 0, memory_order_release);
    atomic_store_explicit(&rb->read_pos,  0, memory_order_release);
}

size_t pcdsp_rb_write_avail(const pcdsp_ringbuffer_t *rb)
{
    uint64_t wp = rb_write_pos(rb);
    uint64_t rp = rb_read_pos(rb);
    return rb->capacity - (size_t)(wp - rp);
}

size_t pcdsp_rb_read_avail(const pcdsp_ringbuffer_t *rb)
{
    uint64_t wp = rb_write_pos(rb);
    uint64_t rp = rb_read_pos(rb);
    return (size_t)(wp - rp);
}

size_t pcdsp_rb_write(pcdsp_ringbuffer_t *rb, const void *src, size_t frames)
{
    uint64_t wp    = atomic_load_explicit(&rb->write_pos, memory_order_relaxed);
    uint64_t rp    = atomic_load_explicit(&rb->read_pos,  memory_order_acquire);
    size_t   avail = rb->capacity - (size_t)(wp - rp);

    if (frames > avail)
        frames = avail;
    if (frames == 0)
        return 0;

    size_t idx1 = (size_t)(wp & rb->mask);
    size_t cont = rb->capacity - idx1;   /* contiguous frames to end of buffer */

    if (frames <= cont) {
        memcpy(rb->buf + idx1 * rb->frame_bytes, src, frames * rb->frame_bytes);
    } else {
        memcpy(rb->buf + idx1 * rb->frame_bytes, src,                  cont * rb->frame_bytes);
        memcpy(rb->buf,                           (const uint8_t *)src + cont * rb->frame_bytes,
               (frames - cont) * rb->frame_bytes);
    }

    atomic_store_explicit(&rb->write_pos, wp + frames, memory_order_release);
    return frames;
}

size_t pcdsp_rb_read(pcdsp_ringbuffer_t *rb, void *dst, size_t frames)
{
    uint64_t rp    = atomic_load_explicit(&rb->read_pos,  memory_order_relaxed);
    uint64_t wp    = atomic_load_explicit(&rb->write_pos, memory_order_acquire);
    size_t   avail = (size_t)(wp - rp);

    if (frames > avail)
        frames = avail;
    if (frames == 0)
        return 0;

    size_t idx1 = (size_t)(rp & rb->mask);
    size_t cont = rb->capacity - idx1;

    if (frames <= cont) {
        memcpy(dst, rb->buf + idx1 * rb->frame_bytes, frames * rb->frame_bytes);
    } else {
        memcpy(dst,                                   rb->buf + idx1 * rb->frame_bytes, cont * rb->frame_bytes);
        memcpy((uint8_t *)dst + cont * rb->frame_bytes, rb->buf,                        (frames - cont) * rb->frame_bytes);
    }

    atomic_store_explicit(&rb->read_pos, rp + frames, memory_order_release);
    return frames;
}

size_t pcdsp_rb_drop(pcdsp_ringbuffer_t *rb, size_t frames)
{
    uint64_t rp    = atomic_load_explicit(&rb->read_pos,  memory_order_relaxed);
    uint64_t wp    = atomic_load_explicit(&rb->write_pos, memory_order_acquire);
    size_t   avail = (size_t)(wp - rp);

    if (frames > avail)
        frames = avail;

    atomic_store_explicit(&rb->read_pos, rp + frames, memory_order_release);
    return frames;
}
