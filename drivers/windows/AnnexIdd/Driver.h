// AnnexIdd - Indirect Display Driver that adds "Annex Display" monitors on request.
// Written against the public IddCx documentation; Microsoft's IndirectDisplay sample was used
// as an architectural reference only.
// SPDX-License-Identifier: MIT
#pragma once

#define NOMINMAX
#include <windows.h>
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <iddcx.h>

#include <avrt.h>
#include <d3d11_2.h>
#include <dxgi1_5.h>
#include <wrl.h>

#include <atomic>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

#include "AnnexIddControl.h"
#include "Edid.h"

namespace annex {

/// Drains a monitor's swap chain so DWM keeps presenting. Pixels are captured in user mode
/// (Windows.Graphics.Capture), so nothing is copied here.
class SwapChainProcessor {
public:
    SwapChainProcessor(IDDCX_SWAPCHAIN swapChain, LUID renderAdapter, HANDLE newFrameEvent);
    ~SwapChainProcessor();

private:
    void Run();
    void RunCore();

    IDDCX_SWAPCHAIN m_swapChain;
    LUID m_renderAdapter;
    HANDLE m_newFrameEvent;
    HANDLE m_terminate;
    std::thread m_thread;
};

struct MonitorEntry {
    UINT connector = 0;
    UINT64 owner = 0;
    EdidSpec spec{};
    std::array<uint8_t, 128> edid{};
    IDDCX_MONITOR monitor = nullptr;
    bool pending = false;  // still being created/announced by AddMonitor
    std::unique_ptr<SwapChainProcessor> processor;
};

/// Per-adapter state shared by the IddCx callbacks and the IOCTL handler.
class Adapter {
public:
    explicit Adapter(WDFDEVICE device);
    ~Adapter();

    NTSTATUS Init();
    void InitFinished(NTSTATUS status);

    NTSTATUS AddMonitor(const ANNEX_ADD_MONITOR& in, ANNEX_ADD_MONITOR_RESULT& out);
    NTSTATUS RemoveMonitor(UINT connector, UINT64 owner);
    void Ping(UINT64 owner);

    /// Modes for the monitor whose EDID has this serial (preferred first).
    std::vector<DISPLAYCONFIG_VIDEO_SIGNAL_INFO> ModesForSerial(uint32_t serial);
    std::vector<DISPLAYCONFIG_VIDEO_SIGNAL_INFO> ModesForMonitor(IDDCX_MONITOR monitor);
    void AssignSwapChain(IDDCX_MONITOR monitor, const IDARG_IN_SETSWAPCHAIN* args);
    void UnassignSwapChain(IDDCX_MONITOR monitor);

private:
    void Watchdog();
    void Depart(std::unique_ptr<MonitorEntry> entry);

    WDFDEVICE m_device;
    IDDCX_ADAPTER m_adapter = nullptr;
    std::atomic<bool> m_ready{false};
    std::mutex m_lock;
    std::vector<std::unique_ptr<MonitorEntry>> m_monitors;
    std::vector<std::pair<UINT64, ULONGLONG>> m_leases;  // owner, last ping (GetTickCount64)
    HANDLE m_stop;
    std::thread m_watchdog;
};

}  // namespace annex

struct DeviceContext {
    annex::Adapter* adapter;
};
WDF_DECLARE_CONTEXT_TYPE(DeviceContext);
