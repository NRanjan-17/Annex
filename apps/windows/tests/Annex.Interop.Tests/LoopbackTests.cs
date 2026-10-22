using System.Collections.Concurrent;
using Annex.Interop;

namespace Annex.Interop.Tests;

/// <summary>Full host ↔ viewer session through the P/Invoke layer and the Rust core.</summary>
public class LoopbackTests
{
    private static T WaitFor<T>(BlockingCollection<CoreEvent> events) where T : CoreEvent
    {
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (DateTime.UtcNow < deadline)
        {
            if (events.TryTake(out var e, TimeSpan.FromMilliseconds(100)) && e is T t) return t;
        }
        throw new TimeoutException($"no {typeof(T).Name}");
    }

    [NativeFact]
    public void HostAndViewerExchangeFrames()
    {
        using var hostCore = CoreClient.Ephemeral("C# Host");
        using var viewerCore = CoreClient.Ephemeral("C# Viewer");
        Assert.Equal(64, hostCore.Fingerprint.Length);
        Assert.Equal("C# Host", hostCore.DeviceName);
        Assert.False(string.IsNullOrEmpty(AnnexNative.Version));

        var hostEvents = new BlockingCollection<CoreEvent>();
        var viewerEvents = new BlockingCollection<CoreEvent>();
        var frames = new BlockingCollection<(uint, bool, byte[])>();

        // PIN pairing: the host shows a PIN; the viewer's "user" types it once both are known.
        var gate = new object();
        string? pin = null;
        ulong? request = null;
        var submitted = false;
        void Note(string? p, ulong? r)
        {
            lock (gate)
            {
                pin ??= p;
                request ??= r;
                if (submitted || pin is null || request is null) return;
                submitted = true;
                Assert.True(viewerCore.SubmitPin(request.Value, pin));
            }
        }

        using var host = new HostHandle(hostCore, 0, 1280, 720, 60, advertise: false, e =>
        {
            if (e is CoreEvent.PairingPin p) Note(p.Pin, null);
            hostEvents.Add(e);
        });
        Assert.NotEqual(0, host.Port);

        var viewer = ViewerHandle.Connect(viewerCore, $"127.0.0.1:{host.Port}", null, new DisplayInfo(1920, 1200),
            e =>
            {
                if (e is CoreEvent.PairingPinRequired r) Note(null, r.RequestId);
                viewerEvents.Add(e);
            },
            (data, id, _, key) => frames.Add((id, key, data.ToArray())));

        Assert.True(WaitFor<CoreEvent.PairingFinished>(viewerEvents).Success);
        var started = WaitFor<CoreEvent.SessionStarted>(hostEvents);
        Assert.Equal("C# Viewer", started.PeerName);
        Assert.Equal("mirror", started.Mode);
        Assert.True(started.SessionId > 0);
        Assert.Equal(1920, started.ViewerDisplay!.WidthPx);
        Assert.Equal(new StreamConfig("h264", 1280, 720, 60), WaitFor<CoreEvent.StreamChanged>(viewerEvents).Stream);
        Assert.Equal(started.SessionId, WaitFor<CoreEvent.KeyframeRequested>(hostEvents).SessionId);

        byte[] au = [0, 0, 0, 1, 0x65, 1, 2, 3];
        Assert.Equal(SubmitResult.Sent, host.Submit(au, AnnexNative.NowMicros(), keyframe: true));
        Assert.True(frames.TryTake(out var f, TimeSpan.FromSeconds(10)));
        Assert.True(f.Item2);
        Assert.Equal(au, f.Item3);

        viewer.Dispose();
        var ended = WaitFor<CoreEvent.SessionEnded>(hostEvents);
        Assert.Equal("user", ended.Reason);
        Assert.Equal(started.SessionId, ended.SessionId);

        Assert.Single(hostCore.TrustedPeers);
        Assert.True(hostCore.Untrust(viewerCore.Fingerprint));
        Assert.Empty(hostCore.TrustedPeers);
    }

    [NativeFact]
    public void ErrorsSurfaceAsExceptions()
    {
        using var core = CoreClient.Ephemeral("x");
        var ex = Assert.Throws<AnnexException>(() => new HostHandle(core, 0, 0, 0, 0, false, _ => { }));
        Assert.Contains("non-zero", ex.Message);
    }
}
