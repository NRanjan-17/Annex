//! User-mode client for the AnnexIdd driver: creates/removes "Annex Display" monitors and keeps
//! their lease alive. Monitors disappear on their own if this process dies (driver watchdog).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ALL_PATHS, QueryDisplayConfig, SDC_APPLY,
    SDC_TOPOLOGY_EXTEND, SetDisplayConfig,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Graphics::Gdi::DISPLAYCONFIG_PATH_ACTIVE;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::{GUID, PCWSTR};

use super::MediaError;
use super::display::{Monitor, monitors};
use crate::idd::{self, AddMonitor, AddMonitorResult, Ping, RemoveMonitor};

struct Handle(HANDLE);
// SAFETY: a file handle can be used from any thread; DeviceIoControl calls are independent.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Connection to the installed AnnexIdd driver.
pub struct VirtualDisplayDriver {
    handle: Arc<Handle>,
    owner: u64,
    stop: Arc<AtomicBool>,
    pinger: Mutex<Option<JoinHandle<()>>>,
    /// Annex targets that already existed when each connector's monitor was added: the new
    /// monitor is the Annex target not in this list.
    known: Mutex<HashMap<u32, Vec<TargetKey>>>,
}

/// A DisplayConfig target: adapter LUID (low, high) and target id.
type TargetKey = (u32, i32, u32);

fn device_path() -> Option<Vec<u16>> {
    let guid = GUID::from_u128(idd::INTERFACE_GUID);
    unsafe {
        let set =
            SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE).ok()?;
        let mut iface = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        let mut path = None;
        if SetupDiEnumDeviceInterfaces(set, None, &guid, 0, &mut iface).is_ok() {
            let mut needed = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &iface, None, 0, Some(&mut needed), None);
            // The detail struct is variable-length; allocate u32-aligned storage.
            let mut buf = vec![0u32; (needed as usize).div_ceil(4).max(2)];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &iface, Some(detail), needed, None, None).is_ok() {
                let start = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
                let max = (needed as usize - std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath)) / 2;
                let chars = std::slice::from_raw_parts(start, max);
                let len = chars.iter().position(|c| *c == 0).unwrap_or(max);
                let mut p = chars[..len].to_vec();
                p.push(0);
                path = Some(p);
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        path
    }
}

impl VirtualDisplayDriver {
    /// True if the AnnexIdd driver is installed and running.
    pub fn available() -> bool {
        device_path().is_some()
    }

    pub fn open() -> Result<Self, MediaError> {
        let path = device_path()
            .ok_or_else(|| MediaError::Other("the Annex virtual display driver is not installed".into()))?;
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )?
        };
        let handle = Arc::new(Handle(handle));
        let owner = {
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u32(std::process::id());
            h.write_u128(Instant::now().elapsed().as_nanos());
            h.finish() | 1
        };
        let stop = Arc::new(AtomicBool::new(false));
        let pinger = {
            let (handle, stop) = (handle.clone(), stop.clone());
            std::thread::Builder::new()
                .name("annex-idd-lease".into())
                .spawn(move || {
                    let ping = Ping { version: idd::PROTOCOL_VERSION, reserved: 0, owner };
                    while !stop.load(Ordering::Acquire) {
                        let _ = ioctl(&handle, idd::IOCTL_PING, &ping, None::<&mut ()>);
                        std::thread::sleep(Duration::from_secs(1));
                    }
                })
                .map_err(|e| MediaError::Other(e.to_string()))?
        };
        Ok(VirtualDisplayDriver { handle, owner, stop, pinger: Mutex::new(Some(pinger)), known: Mutex::default() })
    }

    /// Adds a monitor whose preferred mode is `width`×`height`@`refresh_hz`. Returns its connector index.
    pub fn add_monitor(
        &self,
        serial: u32,
        width: u32,
        height: u32,
        refresh_hz: u32,
        width_mm: u32,
        height_mm: u32,
    ) -> Result<u32, MediaError> {
        let req = AddMonitor {
            version: idd::PROTOCOL_VERSION,
            serial,
            width,
            height,
            refresh_hz,
            width_mm,
            height_mm,
            reserved: 0,
            owner: self.owner,
        };
        let before: Vec<TargetKey> = annex_targets().into_iter().map(|t| t.key).collect();
        let mut out = AddMonitorResult::default();
        ioctl(&self.handle, idd::IOCTL_ADD_MONITOR, &req, Some(&mut out))
            .map_err(|e| MediaError::Other(format!("{e} (an older Annex display driver is still running)")))?;
        if out.status < 0 {
            return Err(MediaError::Other(format!(
                "the display driver could not add a {width}x{height}@{refresh_hz} monitor: {} failed (NTSTATUS 0x{:08X})",
                idd::add_stage_name(out.stage),
                out.status as u32
            )));
        }
        self.known.lock().unwrap().insert(out.connector_index, before);
        Ok(out.connector_index)
    }

    pub fn remove_monitor(&self, connector: u32) -> Result<(), MediaError> {
        let req = RemoveMonitor { version: idd::PROTOCOL_VERSION, connector_index: connector, owner: self.owner };
        ioctl(&self.handle, idd::IOCTL_REMOVE_MONITOR, &req, None::<&mut ()>)
    }

    /// Waits until Windows has activated the monitor and returns it (GDI view of it). If Windows
    /// adds the monitor but leaves it off (a saved "one screen only" layout), the desktop is
    /// switched to Extend once: that's what the user asked for.
    pub fn wait_for_monitor(&self, connector: u32, timeout: Duration) -> Result<Monitor, MediaError> {
        let before = self.known.lock().unwrap().get(&connector).cloned().unwrap_or_default();
        let started = Instant::now();
        let mut extended = false;
        loop {
            let fresh: Vec<AnnexTarget> = annex_targets().into_iter().filter(|t| !before.contains(&t.key)).collect();
            if let Some(name) = fresh.iter().find_map(|t| t.gdi_name.clone())
                && let Some(m) = monitors().into_iter().find(|m| m.name.eq_ignore_ascii_case(&name))
            {
                return Ok(m);
            }
            if !extended && !fresh.is_empty() && started.elapsed() > Duration::from_millis(1500) {
                extended = true;
                // SAFETY: plain call; no buffers.
                let rc = unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_TOPOLOGY_EXTEND) };
                tracing::info!(connector, rc, "extend: the new monitor was off; switched the desktop to Extend");
            }
            if started.elapsed() > timeout {
                let why = if fresh.is_empty() {
                    "Windows did not list the virtual monitor"
                } else {
                    "Windows listed the virtual monitor but did not turn it on"
                };
                return Err(MediaError::Other(format!("the virtual monitor did not become active ({why})")));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for VirtualDisplayDriver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.pinger.lock().unwrap().take() {
            let _ = t.join();
        }
    }
}

fn ioctl<I, O>(h: &Handle, code: u32, input: &I, output: Option<&mut O>) -> Result<(), MediaError> {
    let (out_ptr, out_len) = match output {
        Some(o) => (Some(o as *mut O as *mut core::ffi::c_void), std::mem::size_of::<O>() as u32),
        None => (None, 0),
    };
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            h.0,
            code,
            Some(input as *const I as *const core::ffi::c_void),
            std::mem::size_of::<I>() as u32,
            out_ptr,
            out_len,
            Some(&mut returned),
            None,
        )?;
    }
    Ok(())
}

fn wide_str(buf: &[u16]) -> String {
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

struct AnnexTarget {
    key: TargetKey,
    /// `\\.\DISPLAYn` while the monitor is on (part of the desktop).
    gdi_name: Option<String>,
}

/// Every "Annex Display" target Windows knows, on or off. (DisplayConfig's connectorInstance
/// is a per-adapter count, not the driver's connector index, so targets are told apart by key.)
fn annex_targets() -> Vec<AnnexTarget> {
    let mut out: Vec<AnnexTarget> = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &mut np, &mut nm).is_err() {
            return out;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ALL_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None).is_err() {
            return out;
        }
        for p in &paths[..np as usize] {
            let t = &p.targetInfo;
            let key = (t.adapterId.LowPart, t.adapterId.HighPart, t.id);
            let active = p.flags & DISPLAYCONFIG_PATH_ACTIVE != 0;
            if let Some(existing) = out.iter_mut().find(|x| x.key == key) {
                if !active || existing.gdi_name.is_some() {
                    continue;
                }
            } else {
                let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
                target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
                target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
                target.header.adapterId = t.adapterId;
                target.header.id = t.id;
                if DisplayConfigGetDeviceInfo(&mut target.header) != 0
                    || wide_str(&target.monitorFriendlyDeviceName) != idd::MONITOR_NAME
                {
                    continue;
                }
                out.push(AnnexTarget { key, gdi_name: None });
            }
            if !active {
                continue;
            }
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = p.sourceInfo.adapterId;
            source.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source.header) == 0
                && let Some(entry) = out.iter_mut().find(|x| x.key == key)
            {
                entry.gdi_name = Some(wide_str(&source.viewGdiDeviceName));
            }
        }
    }
    out
}

/// Current pixel size of a monitor by GDI name (follows changes made in Settings).
pub fn monitor_size(name: &str) -> Option<(u32, u32)> {
    monitors().into_iter().find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| (m.width, m.height))
}

/// Why Extend is or isn't available on this PC (for the app to explain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverState {
    /// Installed and running: Extend works.
    Ready,
    /// The device exists but Windows didn't start it. `problem` is its Device Manager problem code
    /// (52 = signature not accepted, typical without test signing).
    NotRunning {
        problem: u32,
    },
    NotInstalled,
}

/// The AnnexIdd device's state and whether Windows test signing is on.
pub fn driver_state() -> (DriverState, bool) {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_PROB, CR_SUCCESS, GUID_DEVCLASS_DISPLAY,
        SETUP_DI_GET_CLASS_DEVS_FLAGS, SP_DEVINFO_DATA, SPDRP_HARDWAREID, SetupDiDestroyDeviceInfoList,
        SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
    };
    let test_signing = test_signing_enabled();
    if VirtualDisplayDriver::available() {
        return (DriverState::Ready, test_signing);
    }
    let mut state = DriverState::NotInstalled;
    unsafe {
        let Ok(set) =
            SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), PCWSTR::null(), None, SETUP_DI_GET_CLASS_DEVS_FLAGS(0))
        else {
            return (state, test_signing);
        };
        for i in 0.. {
            let mut info =
                SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, i, &mut info).is_err() {
                break;
            }
            let mut buf = [0u8; 1024];
            if SetupDiGetDeviceRegistryPropertyW(set, &info, SPDRP_HARDWAREID, None, Some(&mut buf), None).is_err() {
                continue;
            }
            let wide: Vec<u16> = buf.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            if !String::from_utf16_lossy(&wide).split('\0').any(|id| id.eq_ignore_ascii_case("Root\\AnnexIdd")) {
                continue;
            }
            let mut status = CM_DEVNODE_STATUS_FLAGS(0);
            let mut problem = CM_PROB(0);
            let problem = if CM_Get_DevNode_Status(&mut status, &mut problem, info.DevInst, 0) == CR_SUCCESS {
                problem.0
            } else {
                0
            };
            state = DriverState::NotRunning { problem };
            break;
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    (state, test_signing)
}

/// `bcdedit /set testsigning on` shows up as TESTSIGNING in the boot options.
fn test_signing_enabled() -> bool {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            windows::core::w!("SYSTEM\\CurrentControlSet\\Control"),
            windows::core::w!("SystemStartOptions"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .is_ok();
    ok && String::from_utf16_lossy(&buf[..(size as usize / 2)]).to_ascii_uppercase().contains("TESTSIGNING")
}
