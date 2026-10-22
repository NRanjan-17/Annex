/*
 * annex.h - C interface to the Annex core (Rust, crate `annex-ffi`).
 *
 * Conventions
 *  - int32_t results: ANNEX_OK (0) on success, negative on error; annex_last_error()
 *    describes the most recent error on the calling thread.
 *  - `char *` results are owned by the caller; release with annex_string_free().
 *  - Callbacks may run on any thread. Never call an object's *_free function from inside one
 *    of its own callbacks. After *_free returns, that object's callbacks never run again.
 *  - Events are UTF-8 JSON objects with a "type" field (see docs/PROTOCOL.md, "FFI events").
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef ANNEX_H
#define ANNEX_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define ANNEX_OK 0
#define ANNEX_ERR_INVALID_ARGUMENT (-1)
#define ANNEX_ERR_FAILED (-2)
#define ANNEX_ERR_PANIC (-3)

#define ANNEX_SUBMIT_SENT 0
#define ANNEX_SUBMIT_NO_SESSION 1
#define ANNEX_SUBMIT_DROPPED 2

/* capture_ts_local_us value when the clock offset to the host is not known yet. */
#define ANNEX_TS_UNKNOWN INT64_MIN

typedef struct AnnexCore AnnexCore;
typedef struct AnnexHost AnnexHost;
typedef struct AnnexViewer AnnexViewer;
typedef struct AnnexDiscovery AnnexDiscovery;

typedef void (*AnnexEventCallback)(void *ctx, const char *event_json);
typedef void (*AnnexFrameCallback)(void *ctx, const uint8_t *data, size_t len, uint32_t frame_id,
                                   int64_t capture_ts_local_us, bool keyframe);

/* ---- Library ---------------------------------------------------------------------- */
const char *annex_version(void);
const char *annex_last_error(void);
void annex_string_free(char *s);
/* Microseconds on the core's monotonic clock; use for capture timestamps. */
int64_t annex_now_us(void);
/* "AB12 CD34 EF56 7890 1234" display form of a fingerprint. */
char *annex_display_fingerprint(const char *fingerprint);

/* ---- Core: identity + trust store ------------------------------------------------- */
int32_t annex_core_new(const char *data_dir, const char *device_name, AnnexCore **out);
int32_t annex_core_new_ephemeral(const char *device_name, AnnexCore **out);
void annex_core_free(AnnexCore *core);
char *annex_core_fingerprint(const AnnexCore *core);
char *annex_core_device_name(const AnnexCore *core);
/* JSON array of {fingerprint, name, added_at, last_seen}. */
char *annex_core_trusted_peers_json(const AnnexCore *core);
/* Returns 1 if removed, 0 if not trusted. */
int32_t annex_core_untrust(const AnnexCore *core, const char *fingerprint);
/* Answers a "pairing_pin_required" event with the PIN the user typed. Returns 1 if pending. */
int32_t annex_core_submit_pin(const AnnexCore *core, uint64_t request_id, const char *pin);
/* Cancels a pending pairing ("pairing_pin" or "pairing_pin_required"). Returns 1 if pending. */
int32_t annex_core_cancel_request(const AnnexCore *core, uint64_t request_id);

/* ---- Host ------------------------------------------------------------------------- */
/* codec: "h264" | "hevc" | "raw". port 0 picks a free port. */
int32_t annex_host_start(const AnnexCore *core, uint16_t port, const char *codec, uint32_t width,
                         uint32_t height, uint32_t fps, bool advertise, AnnexEventCallback on_event,
                         void *ctx, AnnexHost **out);
uint16_t annex_host_port(const AnnexHost *host);
int32_t annex_host_set_stream(const AnnexHost *host, const char *codec, uint32_t width,
                              uint32_t height, uint32_t fps);
/* One encoded access unit (Annex B). Returns ANNEX_SUBMIT_* or a negative error. */
int32_t annex_host_submit_frame(const AnnexHost *host, const uint8_t *data, size_t len,
                                int64_t capture_ts_us, bool keyframe);
void annex_host_free(AnnexHost *host);
#ifdef _WIN32
/* Windows: JSON array of monitors [{index, name, width, height, refresh_hz, primary}]. */
char *annex_win_monitors_json(void);
/* Windows: host a monitor; the core captures and encodes while a viewer is connected. */
int32_t annex_host_start_screen(const AnnexCore *core, uint32_t monitor_index, uint16_t port, bool advertise,
                                bool prefer_hardware, AnnexEventCallback on_event, void *ctx, AnnexHost **out);
#endif

/* ---- Viewer ----------------------------------------------------------------------- */
/* address: "host", "host:port", "ip" or "ip:port". expected_fingerprint and display_json
 * (a DisplayInfo JSON object) may be NULL. */
int32_t annex_viewer_connect(const AnnexCore *core, const char *address,
                             const char *expected_fingerprint, const char *display_json,
                             AnnexEventCallback on_event, AnnexFrameCallback on_frame, void *ctx,
                             AnnexViewer **out);
#ifdef _WIN32
/* Windows: decode + render in a full-screen window owned by the core. Esc closes it. */
int32_t annex_viewer_connect_windowed(const AnnexCore *core, const char *address,
                                      const char *expected_fingerprint,
                                      AnnexEventCallback on_event, void *ctx, AnnexViewer **out);
#endif
void annex_viewer_request_keyframe(const AnnexViewer *viewer);
void annex_viewer_frame_presented(const AnnexViewer *viewer, uint32_t frame_id);
void annex_viewer_free(AnnexViewer *viewer);

/* ---- Discovery -------------------------------------------------------------------- */
/* Events: {"type":"peer_found",...} and {"type":"peer_lost","instance":...}. */
int32_t annex_discovery_start(const AnnexCore *core, AnnexEventCallback on_event, void *ctx,
                              AnnexDiscovery **out);
void annex_discovery_free(AnnexDiscovery *discovery);

#ifdef __cplusplus
}
#endif
#endif /* ANNEX_H */
