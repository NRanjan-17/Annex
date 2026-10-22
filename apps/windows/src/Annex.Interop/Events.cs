using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

namespace Annex.Interop;

// Mirrors the JSON events from the Rust core (docs/PROTOCOL.md, "FFI events").

public sealed record StreamConfig(string Codec, int Width, int Height, int Fps);

public sealed record DisplayInfo(
    int WidthPx, int HeightPx, int WidthMm = 0, int HeightMm = 0, int RefreshHz = 60, int ScalePercent = 100);

public sealed record LatencyStats(int Frames, double Fps, double P50Ms, double P95Ms, double P99Ms, double MaxMs, double? ClockRttMs = null);

public sealed record PeerInfo(string Instance, string Name, string[] Addresses, int Port, string? Fingerprint = null, int? ProtoMajor = null)
{
    /// <summary>First address as <c>host:port</c> (IPv6 bracketed).</summary>
    public string? ConnectAddress => Addresses.Length == 0 ? null
        : Addresses[0].Contains(':') ? $"[{Addresses[0]}]:{Port}" : $"{Addresses[0]}:{Port}";
}

public sealed record TrustedPeer(string Fingerprint, string Name, ulong AddedAt, ulong LastSeen);

public sealed record MonitorInfo(int Index, string Name, int Width, int Height, int RefreshHz, bool Primary)
{
    public string Label => $"{(Primary ? "Main display" : Name.TrimStart('\\', '.'))} — {Width}×{Height}";
}

public abstract record CoreEvent
{
    public sealed record Listening(int Port) : CoreEvent;
    public sealed record Connecting(string Address) : CoreEvent;
    /// <summary>Host: show this PIN until <see cref="PairingFinished"/> arrives.</summary>
    public sealed record PairingPin(ulong RequestId, string Pin, string PeerName, string FingerprintDisplay) : CoreEvent;
    /// <summary>Viewer: ask the user for the PIN shown on the host.</summary>
    public sealed record PairingPinRequired(ulong RequestId, string PeerName, string FingerprintDisplay) : CoreEvent;
    public sealed record PairingFinished(ulong RequestId, bool Success, string PeerName) : CoreEvent;
    public sealed record SessionStarted(string PeerName, string Fingerprint, StreamConfig Stream, DisplayInfo? ViewerDisplay = null) : CoreEvent;
    public sealed record StreamChanged(StreamConfig Stream) : CoreEvent;
    public sealed record KeyframeRequested : CoreEvent;
    public sealed record Latency(LatencyStats Stats) : CoreEvent;
    public sealed record SessionEnded(string Reason) : CoreEvent;
    public sealed record Error(string Message) : CoreEvent;
    public sealed record PeerFound(PeerInfo Peer) : CoreEvent;
    public sealed record PeerLost(string Instance) : CoreEvent;
    public sealed record Unknown(string Type) : CoreEvent;

    public static CoreEvent Parse(string json)
    {
        string type;
        try
        {
            using var doc = JsonDocument.Parse(json);
            type = doc.RootElement.TryGetProperty("type", out var t) ? t.GetString() ?? "" : "";
        }
        catch (JsonException)
        {
            return new Unknown("invalid");
        }
        try
        {
            return type switch
            {
                "listening" => new Listening(Get<PortDto>(json).Port),
                "connecting" => new Connecting(Get<AddressDto>(json).Address),
                "pairing_pin" => Get<PairingPin>(json),
                "pairing_pin_required" => Get<PairingPinRequired>(json),
                "pairing_finished" => Get<PairingFinished>(json),
                "session_started" => Get<SessionStarted>(json),
                "stream_config" => new StreamChanged(Get<StreamWrapper>(json).Stream),
                "keyframe_requested" => new KeyframeRequested(),
                "latency_stats" => new Latency(Get<LatencyStats>(json)),
                "session_ended" => new SessionEnded(Get<ReasonDto>(json).Reason),
                "error" => new Error(Get<MessageDto>(json).Message),
                "peer_found" => new PeerFound(Get<PeerInfo>(json)),
                "peer_lost" => new PeerLost(Get<InstanceDto>(json).Instance),
                _ => new Unknown(type),
            };
        }
        catch (JsonException)
        {
            return new Unknown(type);
        }
    }

    private static T Get<T>(string json) => AnnexJson.Deserialize<T>(json);
}

internal sealed record StreamWrapper(StreamConfig Stream);
internal sealed record ReasonDto(string Reason);
internal sealed record MessageDto(string Message);
internal sealed record PortDto(int Port);
internal sealed record AddressDto(string Address);
internal sealed record InstanceDto(string Instance);

[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.SnakeCaseLower,
    PropertyNameCaseInsensitive = true,
    DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    RespectRequiredConstructorParameters = true,
    RespectNullableAnnotations = true)]
[JsonSerializable(typeof(CoreEvent.PairingPin))]
[JsonSerializable(typeof(CoreEvent.PairingPinRequired))]
[JsonSerializable(typeof(CoreEvent.PairingFinished))]
[JsonSerializable(typeof(CoreEvent.SessionStarted))]
[JsonSerializable(typeof(LatencyStats))]
[JsonSerializable(typeof(PeerInfo))]
[JsonSerializable(typeof(DisplayInfo))]
[JsonSerializable(typeof(TrustedPeer[]))]
[JsonSerializable(typeof(MonitorInfo[]))]
[JsonSerializable(typeof(StreamWrapper))]
[JsonSerializable(typeof(ReasonDto))]
[JsonSerializable(typeof(MessageDto))]
[JsonSerializable(typeof(PortDto))]
[JsonSerializable(typeof(AddressDto))]
[JsonSerializable(typeof(InstanceDto))]
internal sealed partial class AnnexJsonContext : JsonSerializerContext;

/// <summary>Trim/AOT-safe JSON helpers backed by the source-generated context.</summary>
internal static class AnnexJson
{
    public static T Deserialize<T>(string json)
    {
        var info = (JsonTypeInfo<T>)AnnexJsonContext.Default.GetTypeInfo(typeof(T))!;
        return JsonSerializer.Deserialize(json, info) ?? throw new JsonException($"empty {typeof(T).Name}");
    }

    public static string Serialize<T>(T value)
    {
        var info = (JsonTypeInfo<T>)AnnexJsonContext.Default.GetTypeInfo(typeof(T))!;
        return JsonSerializer.Serialize(value, info);
    }
}
