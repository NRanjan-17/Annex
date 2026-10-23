// SPDX-License-Identifier: MIT
#include "Driver.h"

#include <algorithm>
#include <cstring>

// Define (not just declare) our interface GUID in this one translation unit. initguid.h comes after every other
// header so the SDK's own DEFINE_GUIDs (e.g. winioctl.h) stay declarations.
#include <initguid.h>
DEFINE_GUID(GUID_DEVINTERFACE_ANNEX_IDD, 0x6f2b8e14, 0x3c51, 0x4c0e, 0x9d, 0x3a, 0x7a, 0x1e, 0x5b, 0x0c, 0x4d, 0x21);

using Microsoft::WRL::ComPtr;

namespace annex {

static DISPLAYCONFIG_VIDEO_SIGNAL_INFO SignalInfo(UINT32 width, UINT32 height, UINT32 hz) {
    DISPLAYCONFIG_VIDEO_SIGNAL_INFO s = {};
    s.pixelRate = static_cast<UINT64>(hz) * width * height;
    s.hSyncFreq.Numerator = hz * height;
    s.hSyncFreq.Denominator = 1;
    s.vSyncFreq.Numerator = hz;
    s.vSyncFreq.Denominator = 1;
    s.activeSize.cx = width;
    s.activeSize.cy = height;
    s.totalSize = s.activeSize;
    s.AdditionalSignalInfo.videoStandard = 255;
    s.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    return s;
}

/// Preferred (viewer-native) mode first, then common sizes that fit, at the requested and
/// standard refresh rates. Windows lets the user pick any of these in Settings.
static std::vector<DISPLAYCONFIG_VIDEO_SIGNAL_INFO> BuildModes(const EdidSpec& spec) {
    struct Size {
        UINT32 w, h;
    };
    std::vector<Size> sizes = {{spec.width, spec.height}};
    const Size common[] = {{3840, 2160}, {3440, 1440}, {2880, 1800}, {2560, 1600}, {2560, 1440}, {1920, 1200},
                           {1920, 1080}, {1680, 1050}, {1600, 900},  {1440, 900},  {1366, 768},  {1280, 800},
                           {1280, 720},  {1024, 768}};
    for (const Size& c : common) {
        if (c.w <= spec.width && c.h <= spec.height &&
            std::none_of(sizes.begin(), sizes.end(), [&](const Size& s) { return s.w == c.w && s.h == c.h; })) {
            sizes.push_back(c);
        }
    }
    // Portrait version of the native size, so the display can be rotated in Settings.
    sizes.push_back({spec.height, spec.width});
    std::vector<UINT32> rates = {spec.refreshHz ? spec.refreshHz : 60u};
    if (rates[0] != 60) rates.push_back(60);
    std::vector<DISPLAYCONFIG_VIDEO_SIGNAL_INFO> modes;
    for (const Size& s : sizes)
        for (UINT32 hz : rates) modes.push_back(SignalInfo(s.w, s.h, hz));
    return modes;
}

// ---------------------------------------------------------------------------------------------
// Swap chain processor

SwapChainProcessor::SwapChainProcessor(IDDCX_SWAPCHAIN swapChain, LUID renderAdapter, HANDLE newFrameEvent)
    : m_swapChain(swapChain), m_renderAdapter(renderAdapter), m_newFrameEvent(newFrameEvent) {
    m_terminate = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    m_thread = std::thread([this] { Run(); });
}

SwapChainProcessor::~SwapChainProcessor() {
    SetEvent(m_terminate);
    if (m_thread.joinable()) m_thread.join();
    CloseHandle(m_terminate);
}

void SwapChainProcessor::Run() {
    DWORD taskIndex = 0;
    HANDLE task = AvSetMmThreadCharacteristicsW(L"Distribution", &taskIndex);
    RunCore();
    // The swap chain must always be deleted when we're done with it.
    WdfObjectDelete(reinterpret_cast<WDFOBJECT>(m_swapChain));
    m_swapChain = nullptr;
    if (task) AvRevertMmThreadCharacteristics(task);
}

void SwapChainProcessor::RunCore() {
    ComPtr<IDXGIFactory5> factory;
    if (FAILED(CreateDXGIFactory2(0, IID_PPV_ARGS(&factory)))) return;
    ComPtr<IDXGIAdapter1> adapter;
    if (FAILED(factory->EnumAdapterByLuid(m_renderAdapter, IID_PPV_ARGS(&adapter)))) return;
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    if (FAILED(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                 nullptr, 0, D3D11_SDK_VERSION, &device, nullptr, &context)))
        return;
    ComPtr<IDXGIDevice> dxgiDevice;
    if (FAILED(device.As(&dxgiDevice))) return;
    IDARG_IN_SWAPCHAINSETDEVICE setDevice = {};
    setDevice.pDevice = dxgiDevice.Get();
    if (FAILED(IddCxSwapChainSetDevice(m_swapChain, &setDevice))) return;

    for (;;) {
        IDARG_OUT_RELEASEANDACQUIREBUFFER buffer = {};
        HRESULT hr = IddCxSwapChainReleaseAndAcquireBuffer(m_swapChain, &buffer);
        if (hr == E_PENDING) {
            HANDLE waits[] = {m_newFrameEvent, m_terminate};
            DWORD w = WaitForMultipleObjects(ARRAYSIZE(waits), waits, FALSE, 16);
            if (w == WAIT_OBJECT_0 || w == WAIT_TIMEOUT) continue;
            break;  // terminate or error
        }
        if (FAILED(hr)) break;  // swap chain abandoned (mode change, monitor departure)
        // Frame acquired: nothing to do (user-mode capture reads the composed desktop).
        hr = IddCxSwapChainFinishedProcessingFrame(m_swapChain);
        if (FAILED(hr)) break;
        if (WaitForSingleObject(m_terminate, 0) == WAIT_OBJECT_0) break;
    }
}

// ---------------------------------------------------------------------------------------------
// Adapter

Adapter::Adapter(WDFDEVICE device) : m_device(device) {
    m_stop = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    m_watchdog = std::thread([this] { Watchdog(); });
}

Adapter::~Adapter() {
    SetEvent(m_stop);
    if (m_watchdog.joinable()) m_watchdog.join();
    CloseHandle(m_stop);
}

NTSTATUS Adapter::Init() {
    if (m_adapter) return STATUS_SUCCESS;  // D0Entry runs on every power-up; init once
    IDDCX_ADAPTER_CAPS caps = {};
    caps.Size = sizeof(caps);
    caps.MaxMonitorsSupported = ANNEX_IDD_MAX_MONITORS;
    caps.EndPointDiagnostics.Size = sizeof(caps.EndPointDiagnostics);
    caps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    caps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_OTHER;
    caps.EndPointDiagnostics.pEndPointFriendlyName = L"Annex Virtual Display";
    caps.EndPointDiagnostics.pEndPointManufacturerName = L"Annex";
    caps.EndPointDiagnostics.pEndPointModelName = L"Annex Display";
    IDDCX_ENDPOINT_VERSION version = {};
    version.Size = sizeof(version);
    version.MajorVer = 1;
    caps.EndPointDiagnostics.pFirmwareVersion = &version;
    caps.EndPointDiagnostics.pHardwareVersion = &version;

    IDARG_IN_ADAPTER_INIT init = {};
    init.WdfDevice = m_device;
    init.pCaps = &caps;
    IDARG_OUT_ADAPTER_INIT out = {};
    NTSTATUS status = IddCxAdapterInitAsync(&init, &out);
    if (NT_SUCCESS(status)) m_adapter = out.AdapterObject;
    return status;
}

void Adapter::InitFinished(NTSTATUS status) {
    m_ready = NT_SUCCESS(status);
}

static UINT ConnectorFor(uint32_t serial, const std::vector<std::unique_ptr<MonitorEntry>>& used) {
    // Same viewer → same connector index whenever possible (Windows keys saved layouts on it).
    for (UINT i = 0; i < ANNEX_IDD_MAX_MONITORS; i++) {
        UINT candidate = (serial + i) % ANNEX_IDD_MAX_MONITORS;
        if (std::none_of(used.begin(), used.end(), [&](const auto& m) { return m->connector == candidate; }))
            return candidate;
    }
    return UINT_MAX;
}

NTSTATUS Adapter::AddMonitor(const ANNEX_ADD_MONITOR& in, ANNEX_ADD_MONITOR_RESULT& out) {
    out.Stage = ANNEX_ADD_STAGE_NOT_READY;
    if (!m_ready) return STATUS_DEVICE_NOT_READY;
    out.Stage = ANNEX_ADD_STAGE_VERSION;
    if (in.Version != ANNEX_IDD_PROTOCOL_VERSION) return STATUS_REVISION_MISMATCH;
    out.Stage = ANNEX_ADD_STAGE_SIZE;
    if (in.Width < 640 || in.Height < 480 || in.Width > 8192 || in.Height > 8192) return STATUS_INVALID_PARAMETER;
    out.Stage = ANNEX_ADD_STAGE_NO_CONNECTOR;

    // IddCx calls back into ModesForSerial/ModesForMonitor/AssignSwapChain while it creates and
    // announces the monitor, so the entry is listed first (callbacks see its real size) and the
    // lock is never held across an IddCx call (re-locking std::mutex on one thread throws, which
    // kills the driver host).
    MonitorEntry* entry = nullptr;
    {
        std::lock_guard<std::mutex> guard(m_lock);
        if (m_monitors.size() >= ANNEX_IDD_MAX_MONITORS) return STATUS_TOO_MANY_NODES;
        auto fresh = std::make_unique<MonitorEntry>();
        fresh->connector = ConnectorFor(in.Serial, m_monitors);
        if (fresh->connector == UINT_MAX) return STATUS_TOO_MANY_NODES;
        fresh->owner = in.Owner;
        fresh->pending = true;
        fresh->spec = {in.Serial, in.Width & ~1u, in.Height & ~1u, in.RefreshHz ? in.RefreshHz : 60, in.WidthMm,
                       in.HeightMm};
        fresh->edid = BuildEdid(fresh->spec);
        entry = fresh.get();
        m_monitors.push_back(std::move(fresh));
    }

    IDDCX_MONITOR_INFO info = {};
    info.Size = sizeof(info);
    info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI;
    info.ConnectorIndex = entry->connector;
    info.MonitorDescription.Size = sizeof(info.MonitorDescription);
    info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    info.MonitorDescription.DataSize = static_cast<UINT>(entry->edid.size());
    info.MonitorDescription.pData = entry->edid.data();
    // Stable container ID per viewer.
    info.MonitorContainerId = {0x8a1f2c3e, 0x51d4, 0x4b6a, {0x9e, 0x07, 0, 0, 0, 0, 0, 0}};
    std::memcpy(&info.MonitorContainerId.Data4[4], &in.Serial, 4);

    IDARG_IN_MONITORCREATE create = {};
    create.pMonitorInfo = &info;
    IDARG_OUT_MONITORCREATE created = {};
    out.Stage = ANNEX_ADD_STAGE_CREATE;
    NTSTATUS status = IddCxMonitorCreate(m_adapter, &create, &created);
    if (NT_SUCCESS(status)) {
        out.Stage = ANNEX_ADD_STAGE_ARRIVAL;
        {
            std::lock_guard<std::mutex> guard(m_lock);
            entry->monitor = created.MonitorObject;
        }
        IDARG_OUT_MONITORARRIVAL arrival = {};
        status = IddCxMonitorArrival(created.MonitorObject, &arrival);
        if (!NT_SUCCESS(status)) WdfObjectDelete(reinterpret_cast<WDFOBJECT>(created.MonitorObject));
    }

    std::lock_guard<std::mutex> guard(m_lock);
    if (!NT_SUCCESS(status)) {
        m_monitors.erase(std::find_if(m_monitors.begin(), m_monitors.end(),
                                      [&](const auto& m) { return m.get() == entry; }));
        return status;
    }
    entry->pending = false;
    out.Stage = ANNEX_ADD_STAGE_OK;
    out.ConnectorIndex = entry->connector;
    bool known = false;
    for (auto& lease : m_leases)
        if (lease.first == in.Owner) {
            lease.second = GetTickCount64();
            known = true;
        }
    if (!known) m_leases.emplace_back(in.Owner, GetTickCount64());
    return STATUS_SUCCESS;
}

/// Called without the lock: stopping the swap chain and departing re-enter our IddCx callbacks.
void Adapter::Depart(std::unique_ptr<MonitorEntry> entry) {
    entry->processor.reset();
    if (entry->monitor) IddCxMonitorDeparture(entry->monitor);
}

NTSTATUS Adapter::RemoveMonitor(UINT connector, UINT64 owner) {
    std::unique_ptr<MonitorEntry> removed;
    {
        std::lock_guard<std::mutex> guard(m_lock);
        auto it = std::find_if(m_monitors.begin(), m_monitors.end(), [&](const auto& m) {
            return !m->pending && m->connector == connector && m->owner == owner;
        });
        if (it == m_monitors.end()) return STATUS_NOT_FOUND;
        removed = std::move(*it);
        m_monitors.erase(it);
    }
    Depart(std::move(removed));
    return STATUS_SUCCESS;
}

void Adapter::Ping(UINT64 owner) {
    std::lock_guard<std::mutex> guard(m_lock);
    for (auto& lease : m_leases)
        if (lease.first == owner) lease.second = GetTickCount64();
}

void Adapter::Watchdog() {
    // Remove monitors whose app stopped pinging (crashed or killed): no window is ever stranded
    // on a display nobody is watching.
    while (WaitForSingleObject(m_stop, 1000) == WAIT_TIMEOUT) {
        std::vector<std::unique_ptr<MonitorEntry>> expired;
        {
            std::lock_guard<std::mutex> guard(m_lock);
            const ULONGLONG now = GetTickCount64();
            for (auto lease = m_leases.begin(); lease != m_leases.end();) {
                if (now - lease->second > ANNEX_IDD_LEASE_MS) {
                    const UINT64 owner = lease->first;
                    for (auto m = m_monitors.begin(); m != m_monitors.end();) {
                        if ((*m)->owner == owner && !(*m)->pending) {
                            expired.push_back(std::move(*m));
                            m = m_monitors.erase(m);
                        } else {
                            ++m;
                        }
                    }
                    lease = m_leases.erase(lease);
                } else {
                    ++lease;
                }
            }
        }
        for (auto& entry : expired) Depart(std::move(entry));
    }
}

std::vector<DISPLAYCONFIG_VIDEO_SIGNAL_INFO> Adapter::ModesForSerial(uint32_t serial) {
    std::lock_guard<std::mutex> guard(m_lock);
    for (auto& m : m_monitors)
        if (m->spec.serial == serial) return BuildModes(m->spec);
    return BuildModes({serial, 1920, 1080, 60, 0, 0});
}

std::vector<DISPLAYCONFIG_VIDEO_SIGNAL_INFO> Adapter::ModesForMonitor(IDDCX_MONITOR monitor) {
    std::lock_guard<std::mutex> guard(m_lock);
    for (auto& m : m_monitors)
        if (m->monitor == monitor) return BuildModes(m->spec);
    return BuildModes({0, 1920, 1080, 60, 0, 0});
}

void Adapter::AssignSwapChain(IDDCX_MONITOR monitor, const IDARG_IN_SETSWAPCHAIN* args) {
    std::unique_ptr<SwapChainProcessor> old;
    {
        std::lock_guard<std::mutex> guard(m_lock);
        for (auto& m : m_monitors) {
            if (m->monitor == monitor) {
                old = std::move(m->processor);
                m->processor = std::make_unique<SwapChainProcessor>(args->hSwapChain, args->RenderAdapterLuid,
                                                                    args->hNextSurfaceAvailable);
                return;  // old stops (joins its thread) here, after the lock is released
            }
        }
    }
    WdfObjectDelete(reinterpret_cast<WDFOBJECT>(args->hSwapChain));
}

void Adapter::UnassignSwapChain(IDDCX_MONITOR monitor) {
    std::unique_ptr<SwapChainProcessor> old;
    std::lock_guard<std::mutex> guard(m_lock);
    for (auto& m : m_monitors)
        if (m->monitor == monitor) old = std::move(m->processor);
}

}  // namespace annex

// -------------------------------------------------------------------------------------------------
// WDF / IddCx callbacks

static annex::Adapter* AdapterOf(WDFDEVICE device) {
    return WdfObjectGet_DeviceContext(device)->adapter;
}

static NTSTATUS AnnexD0Entry(WDFDEVICE device, WDF_POWER_DEVICE_STATE) {
    return AdapterOf(device)->Init();
}

// One adapter per device and one device per driver host (UmdfHostProcessSharing disabled), so
// callbacks that only receive IddCx handles find the adapter through this pointer.
static annex::Adapter* g_adapter = nullptr;

static NTSTATUS AnnexAdapterInitFinished(IDDCX_ADAPTER adapter, const IDARG_IN_ADAPTER_INIT_FINISHED* args) {
    UNREFERENCED_PARAMETER(adapter);
    if (g_adapter) g_adapter->InitFinished(args->AdapterInitStatus);
    return STATUS_SUCCESS;
}

static NTSTATUS AnnexParseMonitorDescription(const IDARG_IN_PARSEMONITORDESCRIPTION* in,
                                             IDARG_OUT_PARSEMONITORDESCRIPTION* out) {
    if (!g_adapter || in->MonitorDescription.Type != IDDCX_MONITOR_DESCRIPTION_TYPE_EDID)
        return STATUS_INVALID_PARAMETER;
    const auto serial = annex::EdidSerial(static_cast<const uint8_t*>(in->MonitorDescription.pData),
                                          in->MonitorDescription.DataSize);
    const auto modes = g_adapter->ModesForSerial(serial);
    out->MonitorModeBufferOutputCount = static_cast<UINT>(modes.size());
    if (in->MonitorModeBufferInputCount < modes.size())
        return in->MonitorModeBufferInputCount > 0 ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
    for (size_t i = 0; i < modes.size(); i++) {
        in->pMonitorModes[i] = {};
        in->pMonitorModes[i].Size = sizeof(IDDCX_MONITOR_MODE);
        in->pMonitorModes[i].Origin = IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR;
        in->pMonitorModes[i].MonitorVideoSignalInfo = modes[i];
    }
    out->PreferredMonitorModeIdx = 0;
    return STATUS_SUCCESS;
}

static NTSTATUS AnnexGetDefaultModes(IDDCX_MONITOR monitor, const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* in,
                                     IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* out) {
    const auto modes = g_adapter ? g_adapter->ModesForMonitor(monitor) : std::vector<DISPLAYCONFIG_VIDEO_SIGNAL_INFO>{};
    out->DefaultMonitorModeBufferOutputCount = static_cast<UINT>(modes.size());
    if (in->DefaultMonitorModeBufferInputCount < modes.size())
        return in->DefaultMonitorModeBufferInputCount > 0 ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
    for (size_t i = 0; i < modes.size(); i++) {
        in->pDefaultMonitorModes[i] = {};
        in->pDefaultMonitorModes[i].Size = sizeof(IDDCX_MONITOR_MODE);
        in->pDefaultMonitorModes[i].Origin = IDDCX_MONITOR_MODE_ORIGIN_DRIVER;
        in->pDefaultMonitorModes[i].MonitorVideoSignalInfo = modes[i];
    }
    out->PreferredMonitorModeIdx = 0;
    return STATUS_SUCCESS;
}

static NTSTATUS AnnexQueryTargetModes(IDDCX_MONITOR monitor, const IDARG_IN_QUERYTARGETMODES* in,
                                      IDARG_OUT_QUERYTARGETMODES* out) {
    const auto modes = g_adapter ? g_adapter->ModesForMonitor(monitor) : std::vector<DISPLAYCONFIG_VIDEO_SIGNAL_INFO>{};
    out->TargetModeBufferOutputCount = static_cast<UINT>(modes.size());
    if (in->TargetModeBufferInputCount < modes.size())
        return in->TargetModeBufferInputCount > 0 ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
    for (size_t i = 0; i < modes.size(); i++) {
        in->pTargetModes[i] = {};
        in->pTargetModes[i].Size = sizeof(IDDCX_TARGET_MODE);
        in->pTargetModes[i].TargetVideoSignalInfo.targetVideoSignalInfo = modes[i];
        // Target modes must name their vsync divider (1 = every vsync); monitor modes leave it 0.
        // A zero divider here makes IddCxMonitorArrival fail with STATUS_INVALID_PARAMETER.
        in->pTargetModes[i].TargetVideoSignalInfo.targetVideoSignalInfo.AdditionalSignalInfo.vSyncFreqDivider = 1;
    }
    return STATUS_SUCCESS;
}

static NTSTATUS AnnexCommitModes(IDDCX_ADAPTER, const IDARG_IN_COMMITMODES*) {
    return STATUS_SUCCESS;  // nothing to program: the "hardware" is the user-mode capture
}

static NTSTATUS AnnexAssignSwapChain(IDDCX_MONITOR monitor, const IDARG_IN_SETSWAPCHAIN* args) {
    if (g_adapter) g_adapter->AssignSwapChain(monitor, args);
    return STATUS_SUCCESS;
}

static NTSTATUS AnnexUnassignSwapChain(IDDCX_MONITOR monitor) {
    if (g_adapter) g_adapter->UnassignSwapChain(monitor);
    return STATUS_SUCCESS;
}

static VOID AnnexDeviceIoControl(WDFDEVICE device, WDFREQUEST request, size_t outLen, size_t inLen, ULONG code) {
    annex::Adapter* adapter = AdapterOf(device);
    NTSTATUS status = STATUS_INVALID_DEVICE_REQUEST;
    size_t info = 0;
    void* inBuf = nullptr;
    void* outBuf = nullptr;
    switch (code) {
        case IOCTL_ANNEX_ADD_MONITOR:
            if (inLen < sizeof(ANNEX_ADD_MONITOR) || outLen < sizeof(ANNEX_ADD_MONITOR_RESULT)) {
                status = STATUS_BUFFER_TOO_SMALL;
                break;
            }
            status = WdfRequestRetrieveInputBuffer(request, sizeof(ANNEX_ADD_MONITOR), &inBuf, nullptr);
            if (NT_SUCCESS(status))
                status = WdfRequestRetrieveOutputBuffer(request, sizeof(ANNEX_ADD_MONITOR_RESULT), &outBuf, nullptr);
            if (NT_SUCCESS(status)) {
                ANNEX_ADD_MONITOR add = *static_cast<ANNEX_ADD_MONITOR*>(inBuf);
                ANNEX_ADD_MONITOR_RESULT result = {};
                try {
                    result.Status = adapter->AddMonitor(add, result);
                } catch (...) {
                    // An exception must never leave a driver callback: it would kill the host process.
                    result.Status = STATUS_INTERNAL_ERROR;
                    result.Stage = ANNEX_ADD_STAGE_INTERNAL;
                }
                *static_cast<ANNEX_ADD_MONITOR_RESULT*>(outBuf) = result;
                info = sizeof(result);
                status = STATUS_SUCCESS;  // the outcome is in result.Status / result.Stage
            }
            break;
        case IOCTL_ANNEX_REMOVE_MONITOR:
            status = WdfRequestRetrieveInputBuffer(request, sizeof(ANNEX_REMOVE_MONITOR), &inBuf, nullptr);
            if (NT_SUCCESS(status)) {
                const auto* rm = static_cast<ANNEX_REMOVE_MONITOR*>(inBuf);
                status = adapter->RemoveMonitor(rm->ConnectorIndex, rm->Owner);
            }
            break;
        case IOCTL_ANNEX_PING:
            status = WdfRequestRetrieveInputBuffer(request, sizeof(ANNEX_PING), &inBuf, nullptr);
            if (NT_SUCCESS(status)) adapter->Ping(static_cast<ANNEX_PING*>(inBuf)->Owner);
            break;
        default:
            break;
    }
    WdfRequestCompleteWithInformation(request, status, info);
}

static VOID AnnexDeviceCleanup(WDFOBJECT object) {
    auto* ctx = WdfObjectGet_DeviceContext(static_cast<WDFDEVICE>(object));
    if (ctx->adapter == g_adapter) g_adapter = nullptr;
    delete ctx->adapter;
    ctx->adapter = nullptr;
}

static NTSTATUS AnnexDeviceAdd(WDFDRIVER, PWDFDEVICE_INIT init) {
    WDF_PNPPOWER_EVENT_CALLBACKS pnp;
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnp);
    pnp.EvtDeviceD0Entry = AnnexD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(init, &pnp);

    IDD_CX_CLIENT_CONFIG config;
    IDD_CX_CLIENT_CONFIG_INIT(&config);
    config.EvtIddCxAdapterInitFinished = AnnexAdapterInitFinished;
    config.EvtIddCxParseMonitorDescription = AnnexParseMonitorDescription;
    config.EvtIddCxMonitorGetDefaultDescriptionModes = AnnexGetDefaultModes;
    config.EvtIddCxMonitorQueryTargetModes = AnnexQueryTargetModes;
    config.EvtIddCxAdapterCommitModes = AnnexCommitModes;
    config.EvtIddCxMonitorAssignSwapChain = AnnexAssignSwapChain;
    config.EvtIddCxMonitorUnassignSwapChain = AnnexUnassignSwapChain;
    config.EvtIddCxDeviceIoControl = AnnexDeviceIoControl;
    NTSTATUS status = IddCxDeviceInitConfig(init, &config);
    if (!NT_SUCCESS(status)) return status;

    WDF_OBJECT_ATTRIBUTES attrs;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attrs, DeviceContext);
    attrs.EvtCleanupCallback = AnnexDeviceCleanup;
    WDFDEVICE device = nullptr;
    status = WdfDeviceCreate(&init, &attrs, &device);
    if (!NT_SUCCESS(status)) return status;

    status = IddCxDeviceInitialize(device);
    if (!NT_SUCCESS(status)) return status;
    status = WdfDeviceCreateDeviceInterface(device, &GUID_DEVINTERFACE_ANNEX_IDD, nullptr);
    if (!NT_SUCCESS(status)) return status;

    auto* ctx = WdfObjectGet_DeviceContext(device);
    ctx->adapter = new (std::nothrow) annex::Adapter(device);
    if (!ctx->adapter) return STATUS_INSUFFICIENT_RESOURCES;
    g_adapter = ctx->adapter;
    return STATUS_SUCCESS;
}

extern "C" DRIVER_INITIALIZE DriverEntry;

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driverObject, PUNICODE_STRING registryPath) {
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, AnnexDeviceAdd);
    WDF_OBJECT_ATTRIBUTES attrs;
    WDF_OBJECT_ATTRIBUTES_INIT(&attrs);
    return WdfDriverCreate(driverObject, registryPath, &attrs, &config, WDF_NO_HANDLE);
}
