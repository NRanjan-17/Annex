//! gtk4-layer-shell, which lets a window sit at the top edge of the screen on wlroots desktops and
//! KDE (the drop island). It ships inside Annex (`/usr/lib/annex`), not as a system package, and
//! must load before libwayland, so Annex restarts itself once with it preloaded, as the library's
//! docs describe. Everywhere else (GNOME, X11, or a build without it) Annex runs without it.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use gtk::glib::translate::ToGlibPtr;

use crate::desktop::{self, Session};

/// Set in the restarted process, so it doesn't restart again.
const MARK: &str = "ANNEX_LAYER_SHELL";

/// The bundled library: `<prefix>/lib/annex/` next to `<prefix>/bin/annex`.
fn bundled() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ANNEX_LAYER_SHELL_LIB") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let lib = exe.parent()?.parent()?.join("lib/annex/libgtk4-layer-shell.so.0");
    lib.exists().then_some(lib)
}

/// Call first thing in `main`, before GTK starts: restarts with the library preloaded where it can
/// help, and keeps it out of the programs Annex runs (wl-paste, wlr-randr).
pub fn preload() {
    if std::env::var_os(MARK).is_some() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe {
            std::env::remove_var("LD_PRELOAD");
            std::env::remove_var(MARK);
        }
        tracing::info!("drop: restarted with the layer shell preloaded");
        return;
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none()
        || !matches!(desktop::session(), Session::Wlroots(_) | Session::Kde)
    {
        return;
    }
    let Some(lib) = bundled() else {
        tracing::info!("drop: no bundled layer shell; running without the drop island");
        return;
    };
    let Ok(exe) = std::env::current_exe() else { return };
    tracing::info!("drop: restarting with the layer shell ({})", lib.display());
    let preload = match std::env::var("LD_PRELOAD") {
        Ok(other) if !other.is_empty() => format!("{}:{other}", lib.display()),
        _ => lib.display().to_string(),
    };
    let err = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env("LD_PRELOAD", preload)
        .env(MARK, "1")
        .exec();
    // Only returns if it couldn't restart; carry on without the island.
    tracing::warn!("couldn't restart with the layer shell: {err}");
}

type Win = *mut gtk::ffi::GtkWindow;

pub struct LayerShell {
    init_for_window: unsafe extern "C" fn(Win),
    set_layer: unsafe extern "C" fn(Win, c_int),
    set_anchor: unsafe extern "C" fn(Win, c_int, c_int),
    set_keyboard_mode: unsafe extern "C" fn(Win, c_int),
    set_namespace: unsafe extern "C" fn(Win, *const c_char),
}

const LAYER_OVERLAY: c_int = 3;
const EDGE_TOP: c_int = 2;
const KEYBOARD_NONE: c_int = 0;

/// A function from what's loaded (the preload), as `F`.
///
/// # Safety
/// `F` must be the function's real `extern "C"` signature.
unsafe fn function<F: Copy>(name: &CStr) -> Option<F> {
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*mut c_void>());
    // SAFETY: dlsym with a NUL-terminated name; RTLD_DEFAULT searches everything loaded.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    // SAFETY: per this function's contract.
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) })
}

/// The layer shell, if it's loaded and this compositor supports it. Call after GTK started.
pub fn get() -> Option<&'static LayerShell> {
    static SHELL: OnceLock<Option<LayerShell>> = OnceLock::new();
    SHELL
        .get_or_init(|| {
            // SAFETY: the signatures in gtk4-layer-shell's header.
            unsafe {
                let Some(supported) = function::<unsafe extern "C" fn() -> c_int>(c"gtk_layer_is_supported") else {
                    tracing::debug!("drop: the layer shell isn't loaded");
                    return None;
                };
                if supported() == 0 {
                    tracing::info!("drop: the layer shell is loaded, but this compositor doesn't support it");
                    return None;
                }
                Some(LayerShell {
                    init_for_window: function(c"gtk_layer_init_for_window")?,
                    set_layer: function(c"gtk_layer_set_layer")?,
                    set_anchor: function(c"gtk_layer_set_anchor")?,
                    set_keyboard_mode: function(c"gtk_layer_set_keyboard_mode")?,
                    set_namespace: function(c"gtk_layer_set_namespace")?,
                })
            }
        })
        .as_ref()
}

impl LayerShell {
    /// Makes `window` an overlay at the top centre of the screen that never takes the keyboard.
    /// Call before the window is shown.
    pub fn top_centre(&self, window: &gtk::Window) {
        let w: Win = window.to_glib_none().0;
        // SAFETY: a live, unrealized GtkWindow; the functions come from gtk4-layer-shell.
        unsafe {
            (self.init_for_window)(w);
            (self.set_layer)(w, LAYER_OVERLAY);
            (self.set_anchor)(w, EDGE_TOP, 1);
            (self.set_keyboard_mode)(w, KEYBOARD_NONE);
            (self.set_namespace)(w, c"annex".as_ptr());
        }
    }
}
