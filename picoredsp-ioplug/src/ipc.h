/*
 * picoredsp-ioplug — ALSA ioplug for piCoreDSP
 *
 * ipc.h — plugin ↔ Rust controller IPC over AF_UNIX socket
 *
 * Protocol overview
 * -----------------
 * All messages are fixed-size binary structs with a 1-byte type tag and
 * a 1-byte protocol version field, followed by a type-specific payload.
 * All multi-byte integers are little-endian.
 *
 * Handshake sequence (plugin is client, Rust daemon is server):
 *
 *   Plugin → Daemon   HELLO  (version negotiation)
 *   Daemon → Plugin   HELLO  (echo version or lower negotiated version)
 *   Plugin → Daemon   START  (rate, format, channels)
 *   Daemon → Plugin   READY  (fd passed via SCM_RIGHTS: pipe write end)
 *                     — or —
 *                     ERROR  (reason code)
 *   Plugin → Daemon   STOP   (stream ended)
 *
 * The pipe fd received in READY is the write end of a kernel pipe whose
 * read end is connected to CamillaDSP's stdin.  The plugin writes raw PCM
 * directly into this fd; the Rust daemon is never in the data path.
 *
 * Version negotiation: the plugin sends its PROTOCOL_VERSION; the daemon
 * replies with min(daemon_version, plugin_version).  If the negotiated
 * version is less than PROTOCOL_VERSION_MIN the plugin must disconnect.
 *
 * Disconnect behaviour: if the socket is closed unexpectedly the plugin
 * must stop the active stream cleanly (no silent sample discard) and
 * return -EPIPE to the ALSA layer.
 */

#ifndef PICOREDSP_IPC_H
#define PICOREDSP_IPC_H

#include <stdint.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

/* -----------------------------------------------------------------------
 * Protocol constants
 * ---------------------------------------------------------------------- */

#define PCDSP_IPC_PROTOCOL_VERSION      1u
#define PCDSP_IPC_PROTOCOL_VERSION_MIN  1u

/* Maximum wire size of a single message (bytes). */
#define PCDSP_IPC_MAX_MSG_SIZE  64u

/* Default socket path.  Can be overridden via ALSA config. */
#define PCDSP_IPC_DEFAULT_SOCKET_PATH   "/run/picoredsp/control.sock"

/* IPC timeouts (milliseconds).
 *
 * Normal HELLO/frame I/O should fail quickly, but START -> READY includes
 * writing the runtime config, spawning CamillaDSP and opening the physical
 * ALSA playback device.  That path can legitimately take more than one
 * second on Raspberry Pi / piCorePlayer, so give READY its own budget.
 */
#define PCDSP_IPC_CONNECT_TIMEOUT_MS    2000
#define PCDSP_IPC_IO_TIMEOUT_MS         1000
#define PCDSP_IPC_READY_TIMEOUT_MS      5000

/* -----------------------------------------------------------------------
 * Message type tags  (1 byte)
 * ---------------------------------------------------------------------- */

typedef enum {
    PCDSP_MSG_HELLO  = 0x01,
    PCDSP_MSG_START  = 0x02,
    PCDSP_MSG_STOP   = 0x03,
    PCDSP_MSG_READY  = 0x04,
    PCDSP_MSG_ERROR  = 0x05,
} pcdsp_msg_type_t;

/* -----------------------------------------------------------------------
 * Error codes carried in ERROR messages
 * ---------------------------------------------------------------------- */

typedef enum {
    PCDSP_ERR_OK               = 0,
    PCDSP_ERR_CONFIG           = 1,  /* invalid DSP config             */
    PCDSP_ERR_PLAYBACK_DEVICE  = 2,  /* CamillaDSP cannot open DAC     */
    PCDSP_ERR_PROTOCOL         = 3,  /* protocol version mismatch      */
    PCDSP_ERR_INTERNAL         = 4,  /* unspecified controller error   */
} pcdsp_error_code_t;

/* -----------------------------------------------------------------------
 * Wire message layouts  (packed, little-endian)
 *
 * Common 2-byte header: [type:u8][version:u8]
 * ---------------------------------------------------------------------- */

#pragma pack(push, 1)

typedef struct {
    uint8_t type;       /* PCDSP_MSG_HELLO */
    uint8_t version;    /* PCDSP_IPC_PROTOCOL_VERSION */
} pcdsp_msg_hello_t;

typedef struct {
    uint8_t  type;      /* PCDSP_MSG_START */
    uint8_t  version;
    uint32_t rate;      /* sample rate in Hz */
    uint8_t  format;    /* snd_pcm_format_t cast to u8 */
    uint8_t  channels;  /* 1..255 */
} pcdsp_msg_start_t;

typedef struct {
    uint8_t type;       /* PCDSP_MSG_STOP */
    uint8_t version;
} pcdsp_msg_stop_t;

typedef struct {
    uint8_t type;       /* PCDSP_MSG_READY */
    uint8_t version;
    /* pipe write fd delivered out-of-band via SCM_RIGHTS */
} pcdsp_msg_ready_t;

typedef struct {
    uint8_t type;       /* PCDSP_MSG_ERROR */
    uint8_t version;
    uint8_t code;       /* pcdsp_error_code_t */
} pcdsp_msg_error_t;

#pragma pack(pop)

/* -----------------------------------------------------------------------
 * Connection handle
 * ---------------------------------------------------------------------- */

typedef struct pcdsp_ipc_conn {
    int     fd;           /* connected AF_UNIX socket fd, or -1 */
    uint8_t negotiated_version;
} pcdsp_ipc_conn_t;

/* -----------------------------------------------------------------------
 * API
 * ---------------------------------------------------------------------- */

/*
 * pcdsp_ipc_connect — connect to the Rust daemon socket and perform the
 * HELLO version handshake.
 *
 * socket_path: path to the AF_UNIX socket (NULL → default path).
 * Returns 0 on success, negative errno on failure.
 */
int pcdsp_ipc_connect(pcdsp_ipc_conn_t *conn, const char *socket_path);

/* pcdsp_ipc_close — close the connection and reset the handle. */
void pcdsp_ipc_close(pcdsp_ipc_conn_t *conn);

/*
 * pcdsp_ipc_send_start — send a START message.
 * Returns 0 on success, negative errno on failure.
 */
int pcdsp_ipc_send_start(pcdsp_ipc_conn_t *conn,
                         uint32_t rate,
                         uint8_t  format,
                         uint8_t  channels);

/*
 * pcdsp_ipc_send_stop — send a STOP message.
 * Returns 0 on success, negative errno on failure.
 */
int pcdsp_ipc_send_stop(pcdsp_ipc_conn_t *conn);

/*
 * pcdsp_ipc_recv_ready — wait for a READY or ERROR response.
 *
 * On READY with pipe_fd != NULL (Gate 8+): sets *pipe_fd to the received
 * write-end fd (from SCM_RIGHTS).
 * On READY with pipe_fd == NULL (Gate 7): returns success immediately after
 * receiving the READY message; no SCM_RIGHTS follow-up is expected.
 * On ERROR: sets *error_code to the reason, returns -EPROTO.
 * Returns 0 on success (READY), negative errno on failure.
 */
int pcdsp_ipc_recv_ready(pcdsp_ipc_conn_t   *conn,
                         int                *pipe_fd,
                         pcdsp_error_code_t *error_code);

#ifdef __cplusplus
}
#endif

#endif /* PICOREDSP_IPC_H */
