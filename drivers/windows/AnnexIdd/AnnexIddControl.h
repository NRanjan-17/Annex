// AnnexIddControl.h - control interface between Annex (user mode) and the AnnexIdd driver.
// Mirrored in core/annex-media-win/src/win/virtual_display.rs; keep both in sync.
// SPDX-License-Identifier: MIT
#pragma once

#include <guiddef.h>
#include <winioctl.h>

// {6F2B8E14-3C51-4C0E-9D3A-7A1E5B0C4D21}
#define ANNEX_IDD_INTERFACE_GUID 0x6f2b8e14, 0x3c51, 0x4c0e, 0x9d, 0x3a, 0x7a, 0x1e, 0x5b, 0x0c, 0x4d, 0x21
DEFINE_GUID(GUID_DEVINTERFACE_ANNEX_IDD, ANNEX_IDD_INTERFACE_GUID);

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

typedef struct ANNEX_ADD_MONITOR_RESULT {
    UINT32 ConnectorIndex;  // identifies the monitor in REMOVE and in DisplayConfig (connectorInstance)
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
