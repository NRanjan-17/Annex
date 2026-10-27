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
/* Writes the log to <data_dir>/annex.log while enabled. On by default. */
void annex_set_log_enabled(bool enabled);
/* Verbose logging: everything at trace (each input event, each progress step). Off by default. */
void annex_set_log_verbose(bool verbose);
/* Saves the last few thousand log lines to <data_dir>/crashes/ now (as happens on each error).
 * `why` (may be NULL) goes in the file name. Returns the file (annex_string_free), or NULL. */
char *annex_log_save_recording(const char *why);
/* What's new since last_seen (the version this app last ran; NULL on a first install): JSON array
 * of {version, items: [{kind ("new"|"improved"|"fixed"), title, detail}]}, newest first, "[]" when
 * there's nothing to show. Show it once, then remember annex_version(). annex_string_free. */
char *annex_whats_new_json(const char *last_seen);
/* Every release's notes, newest first, same shape (for a "What's new" button). annex_string_free. */
char *annex_whats_new_all_json(void);
/* Writes one line from the app to the log: level 0 = info, 1 = warning, 2 = error (also saves what
 * led up to it), 3 = debug, 4 = trace (verbose only). Start with the feature in brackets, e.g.
 * "[clipboard] copied text, 12 bytes". Never log what's copied or a secret. */
void annex_log(int32_t level, const char *message);
/* Lists this device under "Nearby" on other computers while Annex is open (as not sharing until a
 * host starts). Call once at startup and again after a rename. */
int32_t annex_core_set_presence(const AnnexCore *core, bool visible);
/* Renames this device; returns the stored name (free with annex_string_free). Advertisements
 * update when hosting restarts. */
char *annex_core_set_device_name(const AnnexCore *core, const char *name);
/* Host: lets a trusted device control this computer's mouse and keyboard (off by default).
 * Returns 1 if changed, 0 if the device isn't trusted. Running sessions follow within ~1 s. */
int32_t annex_core_set_allow_input(const AnnexCore *core, const char *fingerprint, bool allow);
/* Headless (nobody at this screen): pairing PINs also go to <data>/pairing-pin.txt and paired
 * devices may use the mouse and keyboard without being allowed one by one. Returns 0. */
int32_t annex_core_set_headless(const AnnexCore *core, bool on);
/* Where files from the other computer are saved (e.g. Downloads); NULL refuses them (default).
 * Applies to sessions that start afterwards. */
int32_t annex_core_set_downloads(const AnnexCore *core, const char *dir);
/* This platform's key code (macOS virtual key code / Windows scancode, 0xE0xx = extended) for a
 * HID usage from an "input" event, or -1 if there is no such key. */
int32_t annex_key_native_from_hid(uint16_t usage);
/* Answers a "pairing_pin_required" event with the PIN the user typed. Returns 1 if pending. */
int32_t annex_core_submit_pin(const AnnexCore *core, uint64_t request_id, const char *pin);
/* Cancels a pending pairing ("pairing_pin" or "pairing_pin_required"). Returns 1 if pending. */
int32_t annex_core_cancel_request(const AnnexCore *core, uint64_t request_id);

/* ---- Host ------------------------------------------------------------------------- */
/* codec: "h264" | "hevc" | "raw". port 0 picks a free port. extend: accept Extend sessions
 * (the app creates a virtual display per extend session). Up to 4 sessions at once; events
 * carry a "session_id". */
int32_t annex_host_start(const AnnexCore *core, uint16_t port, const char *codec, uint32_t width,
                         uint32_t height, uint32_t fps, bool advertise, bool extend,
                         AnnexEventCallback on_event, void *ctx, AnnexHost **out);
uint16_t annex_host_port(const AnnexHost *host);
int32_t annex_host_set_stream(const AnnexHost *host, const char *codec, uint32_t width,
                              uint32_t height, uint32_t fps);
/* One encoded access unit (Annex B) for every MIRROR session. Returns ANNEX_SUBMIT_* or < 0. */
int32_t annex_host_submit_frame(const AnnexHost *host, const uint8_t *data, size_t len,
                                int64_t capture_ts_us, bool keyframe);
/* One encoded access unit for a single (Extend) session. */
int32_t annex_host_submit_session_frame(const AnnexHost *host, uint64_t session_id, const uint8_t *data,
                                        size_t len, int64_t capture_ts_us, bool keyframe);
/* Changes one session's stream (e.g. its virtual display's resolution changed). */
int32_t annex_host_set_session_stream(const AnnexHost *host, uint64_t session_id, const char *codec,
                                      uint32_t width, uint32_t height, uint32_t fps);
/* Ends one session; its viewer is told "host_ended_session". */
void annex_host_end_session(const AnnexHost *host, uint64_t session_id);
/* Clipboard and files (sessions where both sides run 0.4+ over QUIC). mime: "text/plain;charset=utf-8"
 * or "image/png". send_clipboard returns 1 if sent, 0 if the viewer can't take it or it's too big.
 * send_file sets *out_id (see transfer_started / transfer_progress / transfer_ended events). */
int32_t annex_host_send_clipboard(const AnnexHost *host, uint64_t session_id, const char *mime,
                                  const uint8_t *data, size_t len);
int32_t annex_host_send_file(const AnnexHost *host, uint64_t session_id, const char *path, uint64_t *out_id);
void annex_host_cancel_transfer(const AnnexHost *host, uint64_t session_id, bool incoming, uint64_t id);
/* Shares over the internet with one code via the rendezvous server (wss://...). stun (NULL:
 * Cloudflare's) is host:port. turn_url (NULL or "": none) is the user's own relay, turn:host:port.
 * Events: internet_code, internet_joined, internet_relay, then internet_code_ended. */
int32_t annex_host_share_code(const AnnexHost *host, const char *rendezvous, const char *stun,
                              const char *turn_url, const char *turn_username, const char *turn_password);
/* Stops the code (internet_code_ended "stopped"); connected sessions carry on. */
void annex_host_stop_code(const AnnexHost *host);
void annex_host_free(AnnexHost *host);
#ifdef _WIN32
/* Windows: why Extend is or isn't available here (driver state, test signing). JSON; free it. */
char *annex_win_extend_status_json(void);
/* Windows: graphics cards [{name, luid}] for annex_host_start_screen's gpu_luid (0 = default). */
char *annex_win_gpus_json(void);
#endif
#ifdef _WIN32
/* Windows: JSON array of monitors [{index, name, width, height, refresh_hz, primary}]. */
char *annex_win_monitors_json(void);
/* Windows: host a monitor; the core captures and encodes while a viewer is connected. */
int32_t annex_host_start_screen(const AnnexCore *core, uint32_t monitor_index, uint16_t port, bool advertise,
                                bool prefer_hardware, uint64_t gpu_luid, AnnexEventCallback on_event,
                                void *ctx, AnnexHost **out);
#endif

/* ---- Viewer ----------------------------------------------------------------------- */
/* address: "host", "host:port", "ip" or "ip:port". expected_fingerprint and display_json
 * (a DisplayInfo JSON object) may be NULL. extend: ask the host for a new virtual display
 * instead of mirroring. */
int32_t annex_viewer_connect(const AnnexCore *core, const char *address,
                             const char *expected_fingerprint, const char *display_json, bool extend,
                             bool input, uint32_t quality, AnnexEventCallback on_event, AnnexFrameCallback on_frame, void *ctx,
                             AnnexViewer **out);
/* Connects by a code from annex_host_share_code. stun may be NULL. A wrong, used or expired code
 * ends with session_ended reason "unknown_code". */
int32_t annex_viewer_connect_code(const AnnexCore *core, const char *rendezvous, const char *stun,
                                  const char *code, const char *display_json, bool extend, bool input,
                                  uint32_t quality, AnnexEventCallback on_event, AnnexFrameCallback on_frame,
                                  void *ctx, AnnexViewer **out);
#ifdef _WIN32
/* Windows: decode + render in a full-screen window owned by the core. Esc closes it
 * (Ctrl+Alt+Shift+Q while forwarding input, which the window then does by itself). */
int32_t annex_viewer_connect_windowed(const AnnexCore *core, const char *address,
                                      const char *expected_fingerprint, bool extend, bool input,
                                      uint32_t quality,
                                      AnnexEventCallback on_event, void *ctx, AnnexViewer **out);
/* Windows: annex_viewer_connect_code shown like annex_viewer_connect_windowed. */
int32_t annex_viewer_connect_code_windowed(const AnnexCore *core, const char *rendezvous, const char *stun,
                                           const char *code, bool extend, bool input, uint32_t quality,
                                           AnnexEventCallback on_event, void *ctx, AnnexViewer **out);
#endif
void annex_viewer_request_keyframe(const AnnexViewer *viewer);
void annex_viewer_frame_presented(const AnnexViewer *viewer, uint32_t frame_id);
/* Input (connect with input = true; ignored unless session_started/input_allowed says the host
 * allows it). Pointer positions are normalized to the video: (0,0) top-left, (1,1) bottom-right. */
void annex_viewer_send_mouse_move(const AnnexViewer *viewer, double x, double y);
/* The pointer moved by (dx, dy) pixels, for Extend: the host's pointer can leave the extended
 * display. Only when session_started's pointer_deltas is true. */
void annex_viewer_send_mouse_delta(const AnnexViewer *viewer, int32_t dx, int32_t dy);
/* button: 0 left, 1 right, 2 middle. */
void annex_viewer_send_mouse_button(const AnnexViewer *viewer, uint32_t button, bool down);
/* Pixels; positive dy scrolls up (content moves down), positive dx scrolls left. */
void annex_viewer_send_scroll(const AnnexViewer *viewer, int32_t dx, int32_t dy);
/* Clipboard and files to the host (see annex_host_send_clipboard). The host takes them only while
 * this viewer has control. */
int32_t annex_viewer_send_clipboard(const AnnexViewer *viewer, const char *mime, const uint8_t *data, size_t len);
int32_t annex_viewer_send_file(const AnnexViewer *viewer, const char *path, uint64_t *out_id);
void annex_viewer_cancel_transfer(const AnnexViewer *viewer, bool incoming, uint64_t id);
/* native_code: macOS virtual key code / Windows scancode. Returns 1 if sent, 0 if unmapped. */
int32_t annex_viewer_send_key(const AnnexViewer *viewer, uint32_t native_code, bool down);
/* Direct Wi-Fi link (one side runs a hotspot; credentials travel only inside the session).
 * quality for connect: 0 best, 1 balanced, 2 fast (bitrate ceiling). */
void annex_viewer_request_direct_link(const AnnexViewer *viewer);
int32_t annex_viewer_offer_direct_link(const AnnexViewer *viewer, const char *ssid, const char *passphrase);
int32_t annex_host_offer_direct_link(const AnnexHost *host, uint64_t session_id, const char *ssid,
                                     const char *passphrase);
int32_t annex_host_refuse_direct_link(const AnnexHost *host, uint64_t session_id, const char *reason);
int32_t annex_host_request_direct_link(const AnnexHost *host, uint64_t session_id);
int32_t annex_viewer_refuse_direct_link(const AnnexViewer *viewer, const char *reason);
void annex_viewer_free(AnnexViewer *viewer);

/* ---- Updates ------------------------------------------------------------------------ */
/* The app fetches annex_update_releases_url() (GitHub API, send a User-Agent), asks the core which
 * release to offer, downloads the asset plus SHA256SUMS.txt and SHA256SUMS.txt.sig, and installs
 * only if annex_update_verify() returns ANNEX_OK. */
const char *annex_update_releases_url(void);
/* *out: JSON, `null` if current. asset_suffix: "-macos-universal.dmg", "-x64.exe", "-arm64.exe". */
int32_t annex_update_pick(const char *releases_json, bool beta, const char *asset_suffix, char **out);
int32_t annex_update_verify(const char *sums_path, const char *sig_path, const char *file_path,
                            const char *asset_name);

/* ---- Discovery -------------------------------------------------------------------- */
/* Events: {"type":"peer_found",...} and {"type":"peer_lost","instance":...}. */
int32_t annex_discovery_start(const AnnexCore *core, AnnexEventCallback on_event, void *ctx,
                              AnnexDiscovery **out);
void annex_discovery_refresh(const AnnexDiscovery *discovery);
void annex_discovery_free(AnnexDiscovery *discovery);

#ifdef __cplusplus
}
#endif
#endif /* ANNEX_H */
