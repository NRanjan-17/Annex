using System.Runtime.InteropServices;

namespace Annex.Interop;

/// <summary>Delegate for received video frames. <paramref name="data"/> is only valid during the call.</summary>
public delegate void FrameHandler(ReadOnlySpan<byte> data, uint frameId, long? captureTsLocalUs, bool keyframe);

/// <summary>
/// Pins the managed callbacks for one native object. Native code receives a GCHandle as its
/// context pointer; callbacks arrive on Rust threads and must not block on the UI thread.
/// </summary>
internal sealed unsafe class CallbackTarget : IDisposable
{
    private GCHandle _handle;
    public Action<CoreEvent> OnEvent { get; }
    public FrameHandler? OnFrame { get; }

    public CallbackTarget(Action<CoreEvent> onEvent, FrameHandler? onFrame = null)
    {
        OnEvent = onEvent;
        OnFrame = onFrame;
        _handle = GCHandle.Alloc(this);
    }

    public nint Context => GCHandle.ToIntPtr(_handle);

    public void Dispose()
    {
        if (_handle.IsAllocated) _handle.Free();
    }

    public static delegate* unmanaged<nint, nint, void> EventCallback => &OnEventNative;
    public static delegate* unmanaged<nint, byte*, nuint, uint, long, byte, void> FrameCallback => &OnFrameNative;

    [UnmanagedCallersOnly]
    private static void OnEventNative(nint ctx, nint json)
    {
        try
        {
            if (GCHandle.FromIntPtr(ctx).Target is CallbackTarget t)
                t.OnEvent(CoreEvent.Parse(Marshal.PtrToStringUTF8(json) ?? "{}"));
        }
        catch (Exception e)
        {
            // Exceptions must never unwind into Rust.
            System.Diagnostics.Debug.WriteLine($"Annex event handler failed: {e}");
        }
    }

    [UnmanagedCallersOnly]
    private static void OnFrameNative(nint ctx, byte* data, nuint len, uint frameId, long ts, byte keyframe)
    {
        try
        {
            if (GCHandle.FromIntPtr(ctx).Target is CallbackTarget { OnFrame: { } f })
                f(new ReadOnlySpan<byte>(data, checked((int)len)), frameId, ts == long.MinValue ? null : ts, keyframe != 0);
        }
        catch (Exception e)
        {
            System.Diagnostics.Debug.WriteLine($"Annex frame handler failed: {e}");
        }
    }
}

public static class AnnexNative
{
    public static string Version => Marshal.PtrToStringUTF8(NativeMethods.annex_version()) ?? "";
    public static long NowMicros() => NativeMethods.annex_now_us();
    public static string DisplayFingerprint(string fp) => NativeMethods.TakeString(NativeMethods.annex_display_fingerprint(fp));

    /// <summary>Windows only: connected monitors, main display first.</summary>
    /// <summary>Windows only: graphics cards to choose from for sharing (the first is the default).</summary>
    public static IReadOnlyList<GpuInfo> Gpus() =>
        AnnexJson.Deserialize<GpuInfo[]>(NativeMethods.TakeString(NativeMethods.annex_win_gpus_json()));

    /// <summary>Writes the core's log to annex.log in the data folder while on. Off by default.</summary>
    public static void SetLogEnabled(bool enabled) => NativeMethods.annex_set_log_enabled(enabled);

    public static IReadOnlyList<MonitorInfo> Monitors() =>
        AnnexJson.Deserialize<MonitorInfo[]>(NativeMethods.TakeString(NativeMethods.annex_win_monitors_json()));

    /// <summary>Windows only: whether this PC can add extra displays for viewers (Extend), and why not.</summary>
    public static ExtendStatus ExtendStatus() =>
        AnnexJson.Deserialize<ExtendStatus>(NativeMethods.TakeString(NativeMethods.annex_win_extend_status_json()));
}

/// <summary>Identity and trust store. Create one per process.</summary>
public sealed class CoreClient : IDisposable
{
    internal nint Raw { get; private set; }

    private CoreClient(nint raw) => Raw = raw;

    public static CoreClient Open(string dataDirectory, string deviceName)
    {
        Directory.CreateDirectory(dataDirectory);
        NativeMethods.Check(NativeMethods.annex_core_new(dataDirectory, deviceName, out var raw));
        return new CoreClient(raw);
    }

    /// <summary>In-memory identity (tests).</summary>
    public static CoreClient Ephemeral(string deviceName)
    {
        NativeMethods.Check(NativeMethods.annex_core_new_ephemeral(deviceName, out var raw));
        return new CoreClient(raw);
    }

    public string Fingerprint => NativeMethods.TakeString(NativeMethods.annex_core_fingerprint(Raw));
    public string DeviceName => NativeMethods.TakeString(NativeMethods.annex_core_device_name(Raw));

    public IReadOnlyList<TrustedPeer> TrustedPeers =>
        AnnexJson.Deserialize<TrustedPeer[]>(NativeMethods.TakeString(NativeMethods.annex_core_trusted_peers_json(Raw)));

    public bool Untrust(string fingerprint) => NativeMethods.Check(NativeMethods.annex_core_untrust(Raw, fingerprint)) == 1;

    /// <summary>Lists this device under Nearby on other computers while Annex is open (as not sharing until
    /// a host starts). Call at startup and after a rename.</summary>
    public void SetPresence(bool visible) => NativeMethods.Check(NativeMethods.annex_core_set_presence(Raw, visible));

    /// <summary>Renames this device; returns the name as stored (cleaned and shortened).</summary>
    public string SetDeviceName(string name) => NativeMethods.TakeString(NativeMethods.annex_core_set_device_name(Raw, name));

    /// <summary>Lets a trusted device control this PC's mouse and keyboard (off by default). False if it isn't trusted.</summary>
    public bool SetAllowInput(string fingerprint, bool allow) =>
        NativeMethods.Check(NativeMethods.annex_core_set_allow_input(Raw, fingerprint, allow)) == 1;

    /// <summary>Answers a <see cref="CoreEvent.PairingPinRequired"/> with the PIN the user typed.</summary>
    public bool SubmitPin(ulong requestId, string pin) => NativeMethods.annex_core_submit_pin(Raw, requestId, pin) == 1;

    /// <summary>Cancels a pending pairing on either side.</summary>
    public bool CancelRequest(ulong requestId) => NativeMethods.annex_core_cancel_request(Raw, requestId) == 1;

    public void Dispose()
    {
        if (Raw != 0)
        {
            NativeMethods.annex_core_free(Raw);
            Raw = 0;
        }
    }
}

public enum SubmitResult { Sent, NoSession, Dropped }

public sealed unsafe class HostHandle : IDisposable
{
    private readonly CoreClient _core;
    private readonly CallbackTarget _target;
    private nint _raw;

    private HostHandle(CoreClient core, CallbackTarget target, nint raw)
    {
        _core = core;
        _target = target;
        _raw = raw;
    }

    /// <summary>Windows only: share a monitor. Capture and encoding run in the core.</summary>
    public static HostHandle StartScreen(CoreClient core, int monitorIndex, ushort port, bool advertise, bool preferHardware,
        Action<CoreEvent> onEvent, ulong gpuLuid = 0)
    {
        var target = new CallbackTarget(onEvent);
        if (NativeMethods.annex_host_start_screen(core.Raw, (uint)monitorIndex, port, advertise, preferHardware, gpuLuid,
                CallbackTarget.EventCallback, target.Context, out var raw) < 0)
        {
            target.Dispose();
            throw NativeMethods.LastError();
        }
        return new HostHandle(core, target, raw);
    }

    public HostHandle(CoreClient core, ushort port, int width, int height, int fps, bool advertise, Action<CoreEvent> onEvent,
        string codec = "h264", bool extend = false)
    {
        _core = core;
        _target = new CallbackTarget(onEvent);
        var rc = NativeMethods.annex_host_start(core.Raw, port, codec, (uint)width, (uint)height, (uint)fps, advertise, extend,
            CallbackTarget.EventCallback, _target.Context, out _raw);
        if (rc < 0)
        {
            _target.Dispose();
            throw NativeMethods.LastError();
        }
    }

    public ushort Port => _raw == 0 ? (ushort)0 : NativeMethods.annex_host_port(_raw);

    public SubmitResult Submit(ReadOnlySpan<byte> accessUnit, long captureMicros, bool keyframe)
    {
        if (_raw == 0 || accessUnit.IsEmpty) return SubmitResult.NoSession;
        fixed (byte* p = accessUnit)
        {
            return NativeMethods.annex_host_submit_frame(_raw, p, (nuint)accessUnit.Length, captureMicros, keyframe) switch
            {
                NativeMethods.SubmitSent => SubmitResult.Sent,
                NativeMethods.SubmitDropped => SubmitResult.Dropped,
                _ => SubmitResult.NoSession,
            };
        }
    }

    /// <summary>Answers a DirectLinkRequested event with this PC's hotspot.</summary>
    public bool OfferDirectLink(ulong sessionId, string ssid, string passphrase) =>
        _raw != 0 && NativeMethods.Check(NativeMethods.annex_host_offer_direct_link(_raw, sessionId, ssid, passphrase)) == 1;

    /// <summary>Answers a DirectLinkRequested event that can't be met.</summary>
    public bool RefuseDirectLink(ulong sessionId, string reason) =>
        _raw != 0 && NativeMethods.Check(NativeMethods.annex_host_refuse_direct_link(_raw, sessionId, reason)) == 1;

    public void Dispose()
    {
        if (_raw != 0)
        {
            NativeMethods.annex_host_free(_raw);
            _raw = 0;
        }
        _target.Dispose();
        GC.KeepAlive(_core);
    }
}

/// <summary>Viewer quality choice: a bitrate ceiling for the host (Fast suits Wi-Fi).</summary>
public enum Quality : uint { Best = 0, Balanced = 1, Fast = 2 }

public sealed unsafe class ViewerHandle : IDisposable
{
    private readonly CoreClient _core;
    private readonly CallbackTarget _target;
    private nint _raw;

    private ViewerHandle(CoreClient core, CallbackTarget target)
    {
        _core = core;
        _target = target;
    }

    /// <summary>Connects and delivers encoded frames to <paramref name="onFrame"/>.</summary>
    public static ViewerHandle Connect(CoreClient core, string address, string? expectedFingerprint, DisplayInfo display,
        Action<CoreEvent> onEvent, FrameHandler onFrame, bool extend = false, bool input = false, Quality quality = Quality.Best)
    {
        var v = new ViewerHandle(core, new CallbackTarget(onEvent, onFrame));
        var json = AnnexJson.Serialize(display);
        var rc = NativeMethods.annex_viewer_connect(core.Raw, address, expectedFingerprint, json, extend, input, (uint)quality,
            CallbackTarget.EventCallback, CallbackTarget.FrameCallback, v._target.Context, out v._raw);
        if (rc < 0)
        {
            v._target.Dispose();
            throw NativeMethods.LastError();
        }
        return v;
    }

    /// <summary>Windows: connects and shows the stream in a full-screen window owned by the core (Esc closes).
    /// With <paramref name="input"/>, the window forwards mouse and keyboard once the host allows it
    /// (Ctrl+Alt+Shift+Q then leaves).</summary>
    public static ViewerHandle ConnectWindowed(CoreClient core, string address, string? expectedFingerprint, Action<CoreEvent> onEvent,
        bool extend = false, bool input = false, Quality quality = Quality.Best)
    {
        var v = new ViewerHandle(core, new CallbackTarget(onEvent));
        var rc = NativeMethods.annex_viewer_connect_windowed(core.Raw, address, expectedFingerprint, extend, input, (uint)quality,
            CallbackTarget.EventCallback, v._target.Context, out v._raw);
        if (rc < 0)
        {
            v._target.Dispose();
            throw NativeMethods.LastError();
        }
        return v;
    }

    public void RequestKeyframe()
    {
        if (_raw != 0) NativeMethods.annex_viewer_request_keyframe(_raw);
    }

    /// <summary>Asks the host for a direct Wi-Fi link (answered by DirectLinkOffer / DirectLinkUnavailable).</summary>
    public void RequestDirectLink()
    {
        if (_raw != 0) NativeMethods.annex_viewer_request_direct_link(_raw);
    }

    /// <summary>This PC runs a hotspot: tells the host to join it.</summary>
    public void OfferDirectLink(string ssid, string passphrase)
    {
        if (_raw != 0) NativeMethods.Check(NativeMethods.annex_viewer_offer_direct_link(_raw, ssid, passphrase));
    }

    public void FramePresented(uint frameId)
    {
        if (_raw != 0) NativeMethods.annex_viewer_frame_presented(_raw, frameId);
    }

    /// <summary>Disconnects (blocks briefly while the session closes). Not from inside a callback.</summary>
    public void Dispose()
    {
        if (_raw != 0)
        {
            NativeMethods.annex_viewer_free(_raw);
            _raw = 0;
        }
        _target.Dispose();
        GC.KeepAlive(_core);
    }
}

public sealed unsafe class DiscoveryHandle : IDisposable
{
    private readonly CoreClient _core;
    private readonly CallbackTarget _target;
    private nint _raw;

    public DiscoveryHandle(CoreClient core, Action<CoreEvent> onEvent)
    {
        _core = core;
        _target = new CallbackTarget(onEvent);
        if (NativeMethods.annex_discovery_start(core.Raw, CallbackTarget.EventCallback, _target.Context, out _raw) < 0)
        {
            _target.Dispose();
            throw NativeMethods.LastError();
        }
    }

    public void Dispose()
    {
        if (_raw != 0)
        {
            NativeMethods.annex_discovery_free(_raw);
            _raw = 0;
        }
        _target.Dispose();
        GC.KeepAlive(_core);
    }
}
