using Annex.Interop;

namespace Annex.Interop.Tests;

public class EventTests
{
    [Fact]
    public void ParsesTrustRequest()
    {
        var e = CoreEvent.Parse("""{"type":"trust_request","request_id":7,"peer_name":"Mac","peer_role":"host","fingerprint":"ab","fingerprint_display":"AB"}""");
        var t = Assert.IsType<CoreEvent.TrustRequest>(e);
        Assert.Equal(7ul, t.RequestId);
        Assert.Equal("host", t.PeerRole);
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
    public void PeerAddressFormatting()
    {
        var p = Assert.IsType<CoreEvent.PeerFound>(CoreEvent.Parse("""{"type":"peer_found","instance":"x","name":"Studio","addresses":["fe80::1"],"port":47800,"fingerprint":null,"proto_major":1}"""));
        Assert.Equal("[fe80::1]:47800", p.Peer.ConnectAddress);
        Assert.Equal("10.0.0.2:5", (p.Peer with { Addresses = ["10.0.0.2"], Port = 5 }).ConnectAddress);
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
