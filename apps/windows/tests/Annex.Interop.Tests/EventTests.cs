using Annex.Interop;

namespace Annex.Interop.Tests;

public class EventTests
{
    [Fact]
    public void ParsesPairingEvents()
    {
        var p = Assert.IsType<CoreEvent.PairingPin>(CoreEvent.Parse("""{"type":"pairing_pin","request_id":7,"pin":"123 456","peer_name":"PC","fingerprint_display":"AB"}"""));
        Assert.Equal("123 456", p.Pin);
        var r = Assert.IsType<CoreEvent.PairingPinRequired>(CoreEvent.Parse("""{"type":"pairing_pin_required","request_id":8,"peer_name":"Mac","fingerprint_display":"CD"}"""));
        Assert.Equal(8ul, r.RequestId);
        var f = Assert.IsType<CoreEvent.PairingFinished>(CoreEvent.Parse("""{"type":"pairing_finished","request_id":8,"success":false,"peer_name":"Mac"}"""));
        Assert.False(f.Success);
    }

    [Fact]
    public void ParsesLatencyAndStream()
    {
        var l = Assert.IsType<CoreEvent.Latency>(CoreEvent.Parse("""{"type":"latency_stats","frames":10,"fps":59.9,"p50_ms":12.5,"p95_ms":20,"p99_ms":25,"max_ms":30}"""));
        Assert.Equal(12.5, l.Stats.P50Ms);
        Assert.Null(l.Stats.ClockRttMs);
        var s = Assert.IsType<CoreEvent.StreamChanged>(CoreEvent.Parse("""{"type":"stream_config","stream":{"codec":"h264","width":1920,"height":1080,"fps":60}}"""));
        Assert.Equal(new StreamConfig("h264", 1920, 1080, 60), s.Stream);
    }

    [Fact]
    public void ParsesClipboardAndTransfers()
    {
        var c = Assert.IsType<CoreEvent.ClipboardReceived>(CoreEvent.Parse("""{"type":"clipboard_received","session_id":0,"mime":"text/plain;charset=utf-8","data":"aMOpbGxv"}"""));
        Assert.Equal("héllo", System.Text.Encoding.UTF8.GetString(c.Data));
        var t = Assert.IsType<CoreEvent.TransferStarted>(CoreEvent.Parse("""{"type":"transfer_started","session_id":2,"id":1,"incoming":true,"name":"a.bin","size":5}"""));
        Assert.True(t.Incoming);
        var e = Assert.IsType<CoreEvent.TransferEnded>(CoreEvent.Parse("""{"type":"transfer_ended","session_id":2,"id":1,"incoming":true,"ok":false,"error":"cancelled"}"""));
        Assert.Null(e.Path);
        Assert.Equal("cancelled", e.Problem);
        var s = Assert.IsType<CoreEvent.SessionStarted>(CoreEvent.Parse("""{"type":"session_started","session_id":0,"mode":"mirror","peer_name":"Mac","fingerprint":"ab","stream":{"codec":"h264","width":1,"height":1,"fps":60},"peer_os":"macos"}"""));
        Assert.Equal("macos", s.PeerOs);
    }

    [Fact]
    public void PeerAddressFormatting()
    {
        var p = Assert.IsType<CoreEvent.PeerFound>(CoreEvent.Parse("""{"type":"peer_found","instance":"x","name":"Studio","addresses":["fe80::1"],"port":47800,"fingerprint":null,"proto_major":1}"""));
        Assert.Equal("[fe80::1]:47800", p.Peer.ConnectAddress);
        Assert.False(p.Peer.Mesh);
        var mesh = Assert.IsType<CoreEvent.PeerFound>(CoreEvent.Parse("""{"type":"peer_found","instance":"mesh:ab","name":"Office PC","addresses":["100.70.1.2"],"port":47800,"mesh":true}"""));
        Assert.True(mesh.Peer.Mesh);
        Assert.Equal("10.0.0.2:5", (p.Peer with { Addresses = ["10.0.0.2"], Port = 5 }).ConnectAddress);
        Assert.Equal("10.0.0.2:5,[fe80::2]:5", (p.Peer with { Addresses = ["10.0.0.2", "fe80::2"], Port = 5 }).ConnectAddress);
        var hint = Assert.IsType<CoreEvent.BitrateHint>(CoreEvent.Parse("""{"type":"bitrate_hint","session_id":3,"scale_percent":80}"""));
        Assert.Equal(80u, hint.ScalePercent);
    }

    [Fact]
    public void MonitorLabels()
    {
        Assert.Equal("Main display — 2560×1440", new MonitorInfo(0, @"\\.\DISPLAY1", 2560, 1440, 60, true).Label);
        Assert.Equal("DISPLAY2 — 1920×1080", new MonitorInfo(1, @"\\.\DISPLAY2", 1920, 1080, 60, false).Label);
    }

    [Fact]
    public void ParsesInternetEvents()
    {
        Assert.Equal(new CoreEvent.InternetCode("K7P-4QX", 600), CoreEvent.Parse("""{"type":"internet_code","code":"K7P-4QX","expires_in":600}"""));
        Assert.Equal(new CoreEvent.InternetJoined("Office Mac"), CoreEvent.Parse("""{"type":"internet_joined","peer_name":"Office Mac"}"""));
        Assert.Equal(new CoreEvent.InternetRelay(true, true), CoreEvent.Parse("""{"type":"internet_relay","relayed":true,"limited":true}"""));
        Assert.Equal(new CoreEvent.InternetRelay(true, true, 9, 10),
            CoreEvent.Parse("""{"type":"internet_relay","relayed":true,"limited":true,"relay_left":9,"relay_per_day":10}"""));
        Assert.Equal(new CoreEvent.InternetRelay(false, false, 0, 10),
            CoreEvent.Parse("""{"type":"internet_relay","relayed":false,"limited":false,"relay_left":0,"relay_per_day":10}"""));
        Assert.Equal(new CoreEvent.InternetCodeEnded("expired"), CoreEvent.Parse("""{"type":"internet_code_ended","reason":"expired"}"""));
        var s = Assert.IsType<CoreEvent.SessionStarted>(CoreEvent.Parse(
            """{"type":"session_started","session_id":0,"mode":"mirror","peer_name":"Mac","fingerprint":"ab","stream":{"codec":"h264","width":1,"height":1,"fps":60},"transport":"quic","link":"relayed"}"""));
        Assert.Equal("QUIC · relay", s.ConnectionDescription);
    }

    [Theory]
    [InlineData("""{"type":"from_the_future","x":1}""", "from_the_future")]
    [InlineData("not json", "invalid")]
    [InlineData("""{"type":"listening"}""", "listening")]
    public void UnknownOrMalformedEventsDoNotThrow(string json, string type)
    {
        Assert.Equal(type, Assert.IsType<CoreEvent.Unknown>(CoreEvent.Parse(json)).Type);
    }
}
