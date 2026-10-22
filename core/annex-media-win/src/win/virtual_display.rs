//! User-mode client for the AnnexIdd driver: creates/removes "Annex Display" monitors and keeps
//! their lease alive. Monitors disappear on their own if this process dies (driver watchdog).

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
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
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
}

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
        Ok(VirtualDisplayDriver { handle, owner, stop, pinger: Mutex::new(Some(pinger)) })
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
        let mut out = AddMonitorResult::default();
        ioctl(&self.handle, idd::IOCTL_ADD_MONITOR, &req, Some(&mut out))?;
        Ok(out.connector_index)
    }

    pub fn remove_monitor(&self, connector: u32) -> Result<(), MediaError> {
        let req = RemoveMonitor { version: idd::PROTOCOL_VERSION, connector_index: connector, owner: self.owner };
        ioctl(&self.handle, idd::IOCTL_REMOVE_MONITOR, &req, None::<&mut ()>)
    }

    /// Waits until Windows has activated the monitor and returns it (GDI view of it).
    pub fn wait_for_monitor(&self, connector: u32, timeout: Duration) -> Result<Monitor, MediaError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(name) = gdi_name_for_connector(connector)
                && let Some(m) = monitors().into_iter().find(|m| m.name.eq_ignore_ascii_case(&name))
            {
                return Ok(m);
            }
            if Instant::now() > deadline {
                return Err(MediaError::Other("the virtual monitor did not become active".into()));
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

/// GDI device name (`\\.\DISPLAYn`) of the active Annex monitor on `connector`.
fn gdi_name_for_connector(connector: u32) -> Option<String> {
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).is_err() {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None)
            .is_err()
        {
            return None;
        }
        for p in &paths[..np as usize] {
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = p.targetInfo.adapterId;
            target.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut target.header) != 0 {
                continue;
            }
            if wide_str(&target.monitorFriendlyDeviceName) != idd::MONITOR_NAME || target.connectorInstance != connector
            {
                continue;
            }
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = p.sourceInfo.adapterId;
            source.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source.header) == 0 {
                return Some(wide_str(&source.viewGdiDeviceName));
            }
        }
        None
    }
}

/// Current pixel size of a monitor by GDI name (follows changes made in Settings).
pub fn monitor_size(name: &str) -> Option<(u32, u32)> {
    monitors().into_iter().find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| (m.width, m.height))
}
