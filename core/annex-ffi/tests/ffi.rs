//! Drives a full host ↔ viewer session through the C ABI, the way the native apps do.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use annex_ffi::*;

struct Sink {
    events: Mutex<Sender<serde_json::Value>>,
    frames: Mutex<Sender<Frame>>,
}

unsafe extern "C" fn on_event(ctx: *mut c_void, json: *const c_char) {
    let sink = unsafe { &*(ctx as *const Sink) };
    let s = unsafe { CStr::from_ptr(json) }.to_str().unwrap();
    let _ = sink.events.lock().unwrap().send(serde_json::from_str(s).unwrap());
}

unsafe extern "C" fn on_frame(ctx: *mut c_void, data: *const u8, len: usize, id: u32, _ts: i64, key: bool) {
    let sink = unsafe { &*(ctx as *const Sink) };
    let bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
    let _ = sink.frames.lock().unwrap().send((id, key, bytes));
}

type Frame = (u32, bool, Vec<u8>);

fn sink() -> (Box<Sink>, Receiver<serde_json::Value>, Receiver<Frame>) {
    let (et, er) = channel();
    let (ft, fr) = channel();
    (Box::new(Sink { events: Mutex::new(et), frames: Mutex::new(ft) }), er, fr)
}

fn wait(rx: &Receiver<serde_json::Value>, ty: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(e) = rx.recv_timeout(Duration::from_millis(100))
            && e["type"] == ty
        {
            return e;
        }
    }
    panic!("no {ty} event");
}

fn take_string(p: *mut c_char) -> String {
    assert!(!p.is_null());
    let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
    unsafe { annex_string_free(p) };
    s
}

#[test]
fn host_and_viewer_through_c_abi() {
    // Keep test identities out of the user's Keychain. (Only this test reads the variable.)
    unsafe { std::env::set_var("ANNEX_KEY_STORAGE", "file") };
    unsafe {
        let mut host_core = std::ptr::null_mut();
        let mut viewer_core = std::ptr::null_mut();
        let dir = tempfile::tempdir().unwrap();
        let dir_c = CString::new(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(annex_core_new(dir_c.as_ptr(), c"Host Mac".as_ptr(), &mut host_core), ANNEX_OK);
        assert_eq!(annex_core_new_ephemeral(c"Viewer PC".as_ptr(), &mut viewer_core), ANNEX_OK);
        let host_fp = take_string(annex_core_fingerprint(host_core));
        assert_eq!(host_fp.len(), 64);
        assert_eq!(take_string(annex_core_device_name(host_core)), "Host Mac");

        let (hs, hev, _) = sink();
        let mut host = std::ptr::null_mut();
        assert_eq!(
            annex_host_start(
                host_core,
                0,
                c"h264".as_ptr(),
                1920,
                1080,
                60,
                false,
                true,
                Some(on_event),
                &*hs as *const Sink as *mut c_void,
                &mut host
            ),
            ANNEX_OK
        );
        let port = annex_host_port(host);
        assert_ne!(port, 0);
        assert_eq!(wait(&hev, "listening")["port"], port);

        let (vs, vev, vframes) = sink();
        let addr = CString::new(format!("127.0.0.1:{port}")).unwrap();
        // Not paired yet, so no pinned fingerprint.
        let fp_c = CString::new("").unwrap();
        let display = cr#"{"width_px":2560,"height_px":1440,"scale_percent":100}"#;
        let mut viewer = std::ptr::null_mut();
        assert_eq!(
            annex_viewer_connect(
                viewer_core,
                addr.as_ptr(),
                fp_c.as_ptr(),
                display.as_ptr(),
                false,
                false,
                0,
                Some(on_event),
                Some(on_frame),
                &*vs as *const Sink as *mut c_void,
                &mut viewer
            ),
            ANNEX_OK
        );

        // PIN pairing: the host shows a PIN, the viewer's user types it.
        let shown = wait(&hev, "pairing_pin");
        assert_eq!(shown["peer_name"], "Viewer PC");
        let pin = CString::new(shown["pin"].as_str().unwrap()).unwrap();
        let req = wait(&vev, "pairing_pin_required");
        assert_eq!(req["peer_name"], "Host Mac");
        let id = req["request_id"].as_u64().unwrap();
        assert_eq!(annex_core_submit_pin(viewer_core, id, pin.as_ptr()), 1);
        assert_eq!(annex_core_submit_pin(viewer_core, id, pin.as_ptr()), 0, "request ids are single-use");
        assert_eq!(wait(&vev, "pairing_finished")["success"], true);
        assert_eq!(wait(&hev, "pairing_finished")["success"], true);

        let started = wait(&hev, "session_started");
        assert_eq!(started["viewer_display"]["width_px"], 2560);
        assert_eq!(started["mode"], "mirror");
        let session_id = started["session_id"].as_u64().unwrap();
        assert!(session_id > 0);
        let cfg = wait(&vev, "stream_config");
        assert_eq!(cfg["stream"]["codec"], "h264");
        wait(&hev, "keyframe_requested");

        let payload = b"\x00\x00\x00\x01\x65keyframe";
        assert_eq!(
            annex_host_submit_frame(host, payload.as_ptr(), payload.len(), annex_now_us(), true),
            ANNEX_SUBMIT_SENT
        );
        let (id, key, bytes) = vframes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(key);
        assert_eq!(bytes, payload);
        annex_viewer_frame_presented(viewer, id);
        annex_viewer_request_keyframe(viewer);
        assert_eq!(wait(&hev, "keyframe_requested")["session_id"], session_id);
        // Per-session submission reaches the same viewer.
        assert_eq!(
            annex_host_submit_session_frame(host, session_id, payload.as_ptr(), payload.len(), annex_now_us(), true),
            ANNEX_SUBMIT_SENT
        );
        assert_eq!(vframes.recv_timeout(Duration::from_secs(5)).unwrap().2, payload);
        assert_eq!(
            annex_host_submit_session_frame(host, 9999, payload.as_ptr(), payload.len(), 0, true),
            ANNEX_SUBMIT_NO_SESSION
        );
        assert_eq!(annex_host_set_session_stream(host, session_id, c"h264".as_ptr(), 1280, 720, 60), 1);
        assert_eq!(wait(&vev, "stream_config")["stream"]["width"], 1280);

        // Invalid arguments are reported, not crashes.
        assert_eq!(annex_host_submit_frame(host, std::ptr::null(), 0, 0, false), ANNEX_ERR_INVALID_ARGUMENT);
        assert!(!CStr::from_ptr(annex_last_error()).to_bytes().is_empty());
        assert_eq!(annex_host_set_stream(host, c"vp9".as_ptr(), 1, 1, 1), ANNEX_ERR_INVALID_ARGUMENT);

        annex_viewer_free(viewer);
        let ended = wait(&hev, "session_ended");
        assert_eq!(ended["reason"], "user");
        assert_eq!(ended["session_id"], session_id);

        // Host persisted the viewer in its on-disk trust store; revoke it.
        let peers: serde_json::Value =
            serde_json::from_str(&take_string(annex_core_trusted_peers_json(host_core))).unwrap();
        assert_eq!(peers[0]["name"], "Viewer PC");
        let vfp = CString::new(peers[0]["fingerprint"].as_str().unwrap()).unwrap();
        assert_eq!(annex_core_untrust(host_core, vfp.as_ptr()), 1);
        assert_eq!(annex_core_untrust(host_core, vfp.as_ptr()), 0);

        annex_host_free(host);
        annex_core_free(host_core);
        annex_core_free(viewer_core);
        drop((hs, vs));

        // The log is on without anyone turning it on.
        let log = std::fs::read_to_string(dir.path().join("annex.log")).unwrap();
        assert!(log.contains("logging started"), "{log}");
    }
}

#[test]
fn null_and_bad_arguments() {
    unsafe {
        let mut core = std::ptr::null_mut();
        assert_eq!(annex_core_new(std::ptr::null(), c"x".as_ptr(), &mut core), ANNEX_ERR_INVALID_ARGUMENT);
        assert_eq!(annex_core_new_ephemeral(c"x".as_ptr(), std::ptr::null_mut()), ANNEX_ERR_INVALID_ARGUMENT);
        assert!(annex_core_fingerprint(std::ptr::null()).is_null());
        assert_eq!(annex_core_untrust(std::ptr::null(), c"a".as_ptr()), ANNEX_ERR_INVALID_ARGUMENT);
        assert_eq!(annex_core_cancel_request(std::ptr::null(), 1), ANNEX_ERR_INVALID_ARGUMENT);
        annex_host_free(std::ptr::null_mut());
        annex_viewer_free(std::ptr::null_mut());
        annex_discovery_free(std::ptr::null_mut());
        annex_core_free(std::ptr::null_mut());
        assert_eq!(
            take_string(annex_display_fingerprint(c"abcd1234abcd1234abcd".as_ptr())),
            "ABCD 1234 ABCD 1234 ABCD"
        );
        assert!(!CStr::from_ptr(annex_version()).to_bytes().is_empty());
    }
}

/// Every exported function must be declared in include/annex.h (and vice versa).
#[test]
fn header_declares_every_export() {
    let header = include_str!("../include/annex.h");
    let src = include_str!("../src/lib.rs");
    let exports: Vec<&str> = src
        .split("extern \"C\" fn ")
        .skip(1)
        .filter_map(|s| s.split('(').next())
        .filter(|n| n.starts_with("annex_"))
        .collect();
    assert!(exports.len() > 20, "found only {exports:?}");
    for name in &exports {
        assert!(
            header.contains(&format!(" {name}(")) || header.contains(&format!("*{name}(")),
            "{name} missing from annex.h"
        );
    }
    for decl in header.lines().filter(|l| l.contains("annex_") && l.contains('(') && !l.trim_start().starts_with('*')) {
        let name = decl.split('(').next().unwrap().rsplit([' ', '*']).next().unwrap();
        if name.starts_with("annex_") {
            assert!(exports.contains(&name), "{name} declared in annex.h but not exported");
        }
    }
}
