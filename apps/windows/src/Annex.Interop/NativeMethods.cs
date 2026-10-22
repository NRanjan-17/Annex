using System.Reflection;
using System.Runtime.InteropServices;

namespace Annex.Interop;

/// <summary>
/// P/Invoke declarations for core/annex-ffi/include/annex.h. Keep in sync with the header.
/// The library is <c>annex_ffi.dll</c> on Windows (<c>libannex_ffi.dylib</c>/<c>.so</c> when
/// the interop tests run on macOS/Linux). Set <c>ANNEX_NATIVE_DIR</c> to load it from a
/// specific folder.
/// </summary>
internal static unsafe partial class NativeMethods
{
    private const string Lib = "annex_ffi";

    public const int Ok = 0;
    public const int SubmitSent = 0;
    public const int SubmitNoSession = 1;
    public const int SubmitDropped = 2;

    static NativeMethods()
    {
        NativeLibrary.SetDllImportResolver(typeof(NativeMethods).Assembly, Resolve);
    }

    private static nint Resolve(string name, Assembly assembly, DllImportSearchPath? path)
    {
        if (name != Lib) return 0;
        var dir = Environment.GetEnvironmentVariable("ANNEX_NATIVE_DIR");
        if (string.IsNullOrEmpty(dir)) return 0;
        foreach (var file in new[] { "annex_ffi.dll", "libannex_ffi.dylib", "libannex_ffi.so" })
        {
            var full = Path.Combine(dir, file);
            if (File.Exists(full) && NativeLibrary.TryLoad(full, out var handle)) return handle;
        }
        return 0;
    }

    [LibraryImport(Lib)] public static partial nint annex_version();
    [LibraryImport(Lib)] public static partial nint annex_last_error();
    [LibraryImport(Lib)] public static partial void annex_string_free(nint s);
    [LibraryImport(Lib)] public static partial long annex_now_us();

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial nint annex_display_fingerprint(string fingerprint);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial int annex_core_new(string dataDir, string deviceName, out nint core);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial int annex_core_new_ephemeral(string deviceName, out nint core);

    [LibraryImport(Lib)] public static partial void annex_core_free(nint core);
    [LibraryImport(Lib)] public static partial nint annex_core_fingerprint(nint core);
    [LibraryImport(Lib)] public static partial nint annex_core_device_name(nint core);
    [LibraryImport(Lib)] public static partial nint annex_core_trusted_peers_json(nint core);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial int annex_core_untrust(nint core, string fingerprint);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial int annex_core_submit_pin(nint core, ulong requestId, string pin);

    [LibraryImport(Lib)]
    public static partial int annex_core_cancel_request(nint core, ulong requestId);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial int annex_host_start(nint core, ushort port, string codec, uint width, uint height, uint fps,
        [MarshalAs(UnmanagedType.U1)] bool advertise, delegate* unmanaged<nint, nint, void> onEvent, nint ctx, out nint host);

    [LibraryImport(Lib)] public static partial ushort annex_host_port(nint host);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial int annex_host_set_stream(nint host, string codec, uint width, uint height, uint fps);

    [LibraryImport(Lib)]
    public static partial int annex_host_submit_frame(nint host, byte* data, nuint len, long captureTsUs, [MarshalAs(UnmanagedType.U1)] bool keyframe);

    [LibraryImport(Lib)] public static partial void annex_host_free(nint host);

    /// <summary>Windows only.</summary>
    [LibraryImport(Lib)] public static partial nint annex_win_monitors_json();

    /// <summary>Windows only: the core captures and encodes the monitor while a viewer is connected.</summary>
    [LibraryImport(Lib)]
    public static partial int annex_host_start_screen(nint core, uint monitorIndex, ushort port,
        [MarshalAs(UnmanagedType.U1)] bool advertise, [MarshalAs(UnmanagedType.U1)] bool preferHardware,
        delegate* unmanaged<nint, nint, void> onEvent, nint ctx, out nint host);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial int annex_viewer_connect(nint core, string address, string? expectedFingerprint, string? displayJson,
        delegate* unmanaged<nint, nint, void> onEvent, delegate* unmanaged<nint, byte*, nuint, uint, long, byte, void> onFrame,
        nint ctx, out nint viewer);

    /// <summary>Windows only: the core decodes and renders into its own full-screen window.</summary>
    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial int annex_viewer_connect_windowed(nint core, string address, string? expectedFingerprint,
        delegate* unmanaged<nint, nint, void> onEvent, nint ctx, out nint viewer);

    [LibraryImport(Lib)] public static partial void annex_viewer_request_keyframe(nint viewer);
    [LibraryImport(Lib)] public static partial void annex_viewer_frame_presented(nint viewer, uint frameId);
    [LibraryImport(Lib)] public static partial void annex_viewer_free(nint viewer);

    [LibraryImport(Lib)]
    public static partial int annex_discovery_start(nint core, delegate* unmanaged<nint, nint, void> onEvent, nint ctx, out nint discovery);

    [LibraryImport(Lib)] public static partial void annex_discovery_free(nint discovery);

    public static string TakeString(nint p)
    {
        if (p == 0) return "";
        try { return Marshal.PtrToStringUTF8(p) ?? ""; }
        finally { annex_string_free(p); }
    }

    public static AnnexException LastError() => new(Marshal.PtrToStringUTF8(annex_last_error()) ?? "unknown error");

    public static int Check(int code) => code < 0 ? throw LastError() : code;
}

public sealed class AnnexException(string message) : Exception(message);
