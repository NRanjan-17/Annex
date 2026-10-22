use std::path::Path;
use std::process::Command;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DICD_GENERATE_ID, DIF_REGISTERDEVICE, DiUninstallDevice, GUID_DEVCLASS_DISPLAY, HDEVINFO, INSTALLFLAG_FORCE,
    INSTALLFLAG_NONINTERACTIVE, SETUP_DI_GET_CLASS_DEVS_FLAGS, SP_DEVINFO_DATA, SPDRP_HARDWAREID,
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, SetupDiSetDeviceRegistryPropertyW,
    UpdateDriverForPlugAndPlayDevicesW,
};
use windows::Win32::Foundation::HWND;
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::HARDWARE_ID;

pub fn run(args: &[String]) -> i32 {
    let result = match args.first().map(String::as_str) {
        Some("install") => match args.get(1) {
            Some(inf) => install(Path::new(inf)),
            None => Err("usage: annex-driver install <AnnexIdd.inf>".into()),
        },
        Some("uninstall") => uninstall(),
        Some("status") => {
            let n = devices().map(|d| d.len()).unwrap_or(0);
            println!("{}", if n > 0 { "installed" } else { "not installed" });
            return if n > 0 { 0 } else { 1 };
        }
        _ => Err("usage: annex-driver install <inf> | uninstall | status".into()),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("annex-driver: {e}");
            1
        }
    }
}

fn multi_sz(s: &str) -> Vec<u8> {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.extend([0, 0]);
    wide.iter().flat_map(|c| c.to_le_bytes()).collect()
}

/// Display-class devices whose hardware ID is ours (present or not).
fn devices() -> Result<Vec<(HDEVINFO, SP_DEVINFO_DATA)>, String> {
    unsafe {
        let set =
            SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), PCWSTR::null(), None, SETUP_DI_GET_CLASS_DEVS_FLAGS(0))
                .map_err(|e| e.to_string())?;
        let mut found = Vec::new();
        for i in 0.. {
            let mut info =
                SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, i, &mut info).is_err() {
                break;
            }
            let mut buf = [0u8; 1024];
            if SetupDiGetDeviceRegistryPropertyW(set, &info, SPDRP_HARDWAREID, None, Some(&mut buf), None).is_ok() {
                let wide: Vec<u16> = buf.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                let ids = String::from_utf16_lossy(&wide);
                if ids.split('\0').any(|id| id.eq_ignore_ascii_case(HARDWARE_ID)) {
                    found.push((set, info));
                }
            }
        }
        if found.is_empty() {
            let _ = SetupDiDestroyDeviceInfoList(set);
        }
        Ok(found)
    }
}

fn install(inf: &Path) -> Result<(), String> {
    let inf = std::fs::canonicalize(inf).map_err(|e| format!("{}: {e}", inf.display()))?;
    // Strip the \\?\ prefix canonicalize adds; SetupAPI wants a plain path.
    let inf_path = inf.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    unsafe {
        if devices()?.is_empty() {
            let set = SetupDiCreateDeviceInfoList(Some(&GUID_DEVCLASS_DISPLAY), None).map_err(|e| e.to_string())?;
            let mut info =
                SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            let created = SetupDiCreateDeviceInfoW(
                set,
                w!("Display"),
                &GUID_DEVCLASS_DISPLAY,
                w!("Annex Virtual Display Adapter"),
                None,
                DICD_GENERATE_ID,
                Some(&mut info),
            )
            .and_then(|_| {
                SetupDiSetDeviceRegistryPropertyW(set, &mut info, SPDRP_HARDWAREID, Some(&multi_sz(HARDWARE_ID)))
            })
            .and_then(|_| SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, Some(&info)));
            let _ = SetupDiDestroyDeviceInfoList(set);
            created.map_err(|e| format!("creating the device failed: {e}"))?;
        }
        let mut reboot = BOOL(0);
        UpdateDriverForPlugAndPlayDevicesW(
            None,
            &HSTRING::from(HARDWARE_ID),
            &HSTRING::from(inf_path.as_str()),
            // Never show "can't verify the publisher": fail instead (a dialog would hang silent installs).
            INSTALLFLAG_FORCE | INSTALLFLAG_NONINTERACTIVE,
            Some(&mut reboot),
        )
        .map_err(|e| {
            format!("installing the driver failed: {e} (is the package signed for this PC? see driver-trust.ps1)")
        })?;
        println!("installed{}", if reboot.as_bool() { " (restart required)" } else { "" });
    }
    Ok(())
}

fn uninstall() -> Result<(), String> {
    let found = devices()?;
    let mut sets = Vec::new();
    for (set, info) in &found {
        unsafe {
            if let Err(e) = DiUninstallDevice(HWND::default(), *set, info, 0, None) {
                eprintln!("annex-driver: removing a device failed: {e}");
            }
        }
        if !sets.contains(set) {
            sets.push(*set);
        }
    }
    for set in sets {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(set);
        }
    }
    // Remove the driver package from the driver store (published as oemNN.inf).
    let listing = Command::new("pnputil").args(["/enum-drivers"]).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&listing.stdout);
    for published in published_names(&text, "annexidd.inf") {
        let _ = Command::new("pnputil").args(["/delete-driver", &published, "/uninstall", "/force"]).status();
    }
    println!("uninstalled");
    Ok(())
}

/// Published names (`oemNN.inf`) of driver-store entries whose original name is `original`.
fn published_names(pnputil_output: &str, original: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current: Option<String> = None;
    for line in pnputil_output.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        if key.starts_with("published name") {
            current = Some(value.to_string());
        } else if key.starts_with("original name")
            && value.eq_ignore_ascii_case(original)
            && let Some(p) = current.take()
        {
            names.push(p);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pnputil_listing() {
        let out = "Published Name:     oem12.inf\nOriginal Name:      annexidd.inf\nProvider Name: Annex\n\nPublished Name: oem3.inf\nOriginal Name: other.inf\n";
        assert_eq!(published_names(out, "annexidd.inf"), vec!["oem12.inf"]);
    }

    #[test]
    fn multi_sz_is_double_terminated() {
        let b = multi_sz("A");
        assert_eq!(b, vec![b'A', 0, 0, 0, 0, 0]);
    }
}
