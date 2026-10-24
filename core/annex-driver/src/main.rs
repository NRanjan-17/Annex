//! `annex-driver`: installs or removes the AnnexIdd virtual display driver (Windows, elevated).
//!
//! ```text
//! annex-driver install <path\to\AnnexIdd.inf>   create the Root\AnnexIdd device and install the driver
//! annex-driver uninstall                        remove the device(s) and the driver package
//! annex-driver status                           is the device present?
//! ```
//! Used by the Windows installer. Unsigned / test-signed packages only install with test signing on.

// Used by `win`; elsewhere only its tests run.
#[cfg_attr(not(windows), allow(dead_code))]
mod pnputil;
#[cfg(windows)]
mod win;

pub const HARDWARE_ID: &str = "Root\\AnnexIdd";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    std::process::exit(win::run(&args));
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!("annex-driver: Windows only");
        std::process::exit(2);
    }
}
