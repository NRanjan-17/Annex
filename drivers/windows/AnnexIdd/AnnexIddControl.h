// AnnexIddControl.h - control interface between Annex (user mode) and the AnnexIdd driver.
// Mirrored in core/annex-media-win/src/win/virtual_display.rs; keep both in sync.
// SPDX-License-Identifier: MIT
#pragma once

#include <guiddef.h>
#include <winioctl.h>

// {6F2B8E14-3C51-4C0E-9D3A-7A1E5B0C4D21} (also defined in Driver.cpp; keep both in sync)
DEFINE_GUID(GUID_DEVINTERFACE_ANNEX_IDD, 0x6f2b8e14, 0x3c51, 0x4c0e, 0x9d, 0x3a, 0x7a, 0x1e, 0x5b, 0x0c, 0x4d, 0x21);

#define ANNEX_IDD_PROTOCOL_VERSION 1
#define ANNEX_IDD_MAX_MONITORS 4
/// Monitors whose owner hasn't pinged for this long are removed (crash safety).
#define ANNEX_IDD_LEASE_MS 5000

#define IOCTL_ANNEX_ADD_MONITOR CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_ANNEX_REMOVE_MONITOR CTL_CODE(FILE_DEVICE_UNKNOWN, 0x902, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_ANNEX_PING CTL_CODE(FILE_DEVICE_UNKNOWN, 0x903, METHOD_BUFFERED, FILE_ANY_ACCESS)

#pragma pack(push, 8)
typedef struct ANNEX_ADD_MONITOR {
    UINT32 Version;    // ANNEX_IDD_PROTOCOL_VERSION
    UINT32 Serial;     // stable per viewer -> Windows remembers position/resolution/scale
    UINT32 Width;      // preferred (native) mode, pixels
    UINT32 Height;
    UINT32 RefreshHz;
    UINT32 WidthMm;    // physical size (0 = derive from 96 DPI)
    UINT32 HeightMm;
    UINT32 Reserved;
    UINT64 Owner;      // random per app instance; see IOCTL_ANNEX_PING
} ANNEX_ADD_MONITOR;

// Failure details come back in the result (the IOCTL itself then succeeds), because Windows only
// passes an NTSTATUS to user mode as a Win32 code, and many different failures read the same there.
#define ANNEX_ADD_STAGE_OK 0
#define ANNEX_ADD_STAGE_NOT_READY 1
#define ANNEX_ADD_STAGE_VERSION 2
#define ANNEX_ADD_STAGE_SIZE 3
#define ANNEX_ADD_STAGE_NO_CONNECTOR 4
#define ANNEX_ADD_STAGE_CREATE 5   // IddCxMonitorCreate
#define ANNEX_ADD_STAGE_ARRIVAL 6  // IddCxMonitorArrival
#define ANNEX_ADD_STAGE_INTERNAL 7

typedef struct ANNEX_ADD_MONITOR_RESULT {
    UINT32 ConnectorIndex;  // identifies the monitor in REMOVE and in DisplayConfig (connectorInstance)
    INT32 Status;           // NTSTATUS of the failed step; 0 on success
    UINT32 Stage;           // ANNEX_ADD_STAGE_*
    UINT32 Reserved;
} ANNEX_ADD_MONITOR_RESULT;

typedef struct ANNEX_REMOVE_MONITOR {
    UINT32 Version;
    UINT32 ConnectorIndex;
    UINT64 Owner;
} ANNEX_REMOVE_MONITOR;

typedef struct ANNEX_PING {
    UINT32 Version;
    UINT32 Reserved;
    UINT64 Owner;
} ANNEX_PING;
#pragma pack(pop)
