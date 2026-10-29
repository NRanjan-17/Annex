using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

namespace Annex.Interop;

// Mirrors the JSON events from the Rust core (docs/PROTOCOL.md, "FFI events").

public sealed record StreamConfig(string Codec, int Width, int Height, int Fps);

public sealed record DisplayInfo(
    int WidthPx, int HeightPx, int WidthMm = 0, int HeightMm = 0, int RefreshHz = 60, int ScalePercent = 100);

public sealed record LatencyStats(int Frames, double Fps, double P50Ms, double P95Ms, double P99Ms, double MaxMs, double? ClockRttMs = null);

/// <summary><c>Sharing</c>: accepting viewers now (false = Annex is open there but not sharing).
/// <c>Extend</c>: it can add an extra display (null if it doesn't say). <c>Mesh</c>: found on a
/// private network (a mesh VPN), not the local one.</summary>
public sealed record PeerInfo(string Instance, string Name, string[] Addresses, int Port, string? Fingerprint = null, int? ProtoMajor = null,
    bool Sharing = true, bool? Extend = null, bool Mesh = false)
{
    /// <summary>Every address as <c>host:port</c> (IPv6 bracketed), comma-separated; the core races them and
    /// prefers a direct cable.</summary>
    public string? ConnectAddress => Addresses.Length == 0 ? null
        : string.Join(",", Addresses.Select(a => a.Contains(':') ? $"[{a}]:{Port}" : $"{a}:{Port}"));
}

/// <summary>Windows: <c>State</c> is "ready", "not_running" (installed but not loaded; <c>Problem</c> is the
/// Device Manager code) or "not_installed".</summary>
public sealed record ExtendStatus(string State, uint Problem, bool TestSigning);

/// <summary>A graphics card: <c>Luid</c> identifies it for sharing (0 = Windows' default).</summary>
public sealed record GpuInfo(string Name, ulong Luid);

/// <summary>A release worth offering (see annex_update_pick). <c>Downgrade</c>: back from a
/// pre-release to the latest normal release.</summary>
public sealed record UpdateInfo(string Version, bool Prerelease, bool Downgrade, string Notes, string PageUrl,
    string AssetName, string AssetUrl, string SumsUrl, string SigUrl);

/// <summary>One release's notes (see annex_whats_new_json). <c>Kind</c>: "new", "improved" or "fixed".</summary>
public sealed record WhatsNewRelease(string Version, WhatsNewItem[] Items);

public sealed record WhatsNewItem(string Kind, string Title, string Detail);

/// <summary>Host side: <c>AllowInput</c>, the device may control this PC's mouse and keyboard;
/// <c>AllowSound</c>, it hears this PC's sound; <c>AllowMic</c>, its microphone plays here.</summary>
public sealed record TrustedPeer(string Fingerprint, string Name, ulong AddedAt, ulong LastSeen, bool AllowInput = false,
    bool AllowSound = true, bool AllowMic = true);

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
    /// <summary><c>SessionId</c> is 0 on the viewer side; <c>Mode</c> is "mirror" or "extend".</summary>
    public sealed record SessionStarted(ulong SessionId, string Mode, string PeerName, string Fingerprint, StreamConfig Stream,
        DisplayInfo? ViewerDisplay = null, string? Transport = null, string? Link = null, bool Input = false,
        string? PeerOs = null) : CoreEvent
    {
        /// <summary>e.g. "QUIC · direct cable".</summary>
        public string ConnectionDescription => string.Join(" · ", new[]
        {
            Transport switch { "quic" => "QUIC", "tcp" => "TCP", _ => null },
            Link switch { "direct" => "direct cable", "network" => "network", "internet" => "internet", "relayed" => "relay", _ => null },
        }.Where(s => s is not null));
    }
    public sealed record StreamChanged(StreamConfig Stream) : CoreEvent;
    public sealed record KeyframeRequested(ulong SessionId) : CoreEvent;
    /// <summary>Adaptive bitrate: run the session's encoder at ScalePercent % of its normal bitrate.</summary>
    public sealed record BitrateHint(ulong SessionId, uint ScalePercent) : CoreEvent;
    public sealed record Latency(LatencyStats Stats) : CoreEvent;
    /// <summary>Input was allowed or revoked during a session (the core injects it itself on Windows).</summary>
    public sealed record InputAllowed(ulong SessionId, bool Allowed) : CoreEvent;
    /// <summary>Host: the viewer wants to control this PC but isn't allowed yet; ask the user.</summary>
    public sealed record InputRequested(ulong SessionId, string PeerName, string Fingerprint) : CoreEvent;
    /// <summary>Host: the viewer asks for a direct Wi-Fi link.</summary>
    public sealed record DirectLinkRequested(ulong SessionId) : CoreEvent;
    /// <summary>Join this hotspot for a direct link (credentials arrive only inside the session).</summary>
    public sealed record DirectLinkOffer(ulong SessionId, string Ssid, string Passphrase) : CoreEvent;
    public sealed record DirectLinkUnavailable(ulong SessionId, string Reason) : CoreEvent;
    public sealed record SessionEnded(ulong SessionId, string Reason) : CoreEvent;
    public sealed record Error(string Message) : CoreEvent;
    public sealed record PeerFound(PeerInfo Peer) : CoreEvent;
    public sealed record PeerLost(string Instance) : CoreEvent;
    /// <summary>Host: the code to read out; it works once, for <c>ExpiresIn</c> seconds.</summary>
    public sealed record InternetCode(string Code, int ExpiresIn) : CoreEvent;
    /// <summary>Host: someone typed the code; their connection comes next.</summary>
    public sealed record InternetJoined(string PeerName) : CoreEvent;
    /// <summary>Host: a direct connection failed. <c>Relayed</c>: a relay is set up; <c>Limited</c>: it's ours (capped).
    /// When ours was asked, <c>RelayLeft</c> of <c>RelayPerDay</c> sessions on it are left today (0: used up).</summary>
    public sealed record InternetRelay(bool Relayed, bool Limited, int? RelayLeft = null, int? RelayPerDay = null) : CoreEvent;
    /// <summary>Host: the code is done ("used", "expired", "stopped", or an error).</summary>
    public sealed record InternetCodeEnded(string Reason) : CoreEvent;
    /// <summary>The other computer copied text ("text/plain;charset=utf-8") or a picture ("image/png").</summary>
    public sealed record ClipboardReceived(ulong SessionId, string Mime, byte[] Data) : CoreEvent;
    /// <summary>A file started going either way. <c>Incoming</c>: from the other computer to this one.</summary>
    public sealed record TransferStarted(ulong SessionId, ulong Id, bool Incoming, string Name, ulong Size) : CoreEvent;
    public sealed record TransferProgress(ulong SessionId, ulong Id, bool Incoming, ulong Done) : CoreEvent;
    /// <summary><c>Path</c>: where a received file was saved; <c>Problem</c>: why it didn't arrive.</summary>
    public sealed record TransferEnded(ulong SessionId, ulong Id, bool Incoming, bool Ok, string? Path = null,
        [property: JsonPropertyName("error")] string? Problem = null) : CoreEvent;
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
            AnnexLog.Warn($"[app] couldn't read an event from the core ({json.Length} bytes)");
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
                "keyframe_requested" => Get<KeyframeRequested>(json),
                "bitrate_hint" => Get<BitrateHint>(json),
                "latency_stats" => new Latency(Get<LatencyStats>(json)),
                "input_allowed" => Get<InputAllowed>(json),
                "direct_link_requested" => Get<DirectLinkRequested>(json),
                "input_requested" => Get<InputRequested>(json),
                "direct_link_offer" => Get<DirectLinkOffer>(json),
                "direct_link_unavailable" => Get<DirectLinkUnavailable>(json),
                // Windows injects input inside the core; the app doesn't need each event.
                "input" => new Unknown(type),
                "session_ended" => Get<SessionEnded>(json),
                "error" => new Error(Get<MessageDto>(json).Message),
                "peer_found" => new PeerFound(Get<PeerInfo>(json)),
                "peer_lost" => new PeerLost(Get<InstanceDto>(json).Instance),
                "internet_code" => Get<InternetCode>(json),
                "internet_joined" => Get<InternetJoined>(json),
                "internet_relay" => Get<InternetRelay>(json),
                "internet_code_ended" => Get<InternetCodeEnded>(json),
                "clipboard_received" => Get<ClipboardReceived>(json),
                "transfer_started" => Get<TransferStarted>(json),
                "transfer_progress" => Get<TransferProgress>(json),
                "transfer_ended" => Get<TransferEnded>(json),
                _ => new Unknown(type),
            };
        }
        catch (JsonException e)
        {
            AnnexLog.Warn($"[app] couldn't read a \"{type}\" event from the core: {e.Message}");
            return new Unknown(type);
        }
    }

    private static T Get<T>(string json) => AnnexJson.Deserialize<T>(json);
}

internal sealed record StreamWrapper(StreamConfig Stream);
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
[JsonSerializable(typeof(CoreEvent.InputAllowed))]
[JsonSerializable(typeof(CoreEvent.DirectLinkRequested))]
[JsonSerializable(typeof(CoreEvent.InputRequested))]
[JsonSerializable(typeof(CoreEvent.DirectLinkOffer))]
[JsonSerializable(typeof(CoreEvent.DirectLinkUnavailable))]
[JsonSerializable(typeof(CoreEvent.InternetCode))]
[JsonSerializable(typeof(CoreEvent.InternetJoined))]
[JsonSerializable(typeof(CoreEvent.InternetRelay))]
[JsonSerializable(typeof(CoreEvent.InternetCodeEnded))]
[JsonSerializable(typeof(CoreEvent.ClipboardReceived))]
[JsonSerializable(typeof(CoreEvent.TransferStarted))]
[JsonSerializable(typeof(CoreEvent.TransferProgress))]
[JsonSerializable(typeof(CoreEvent.TransferEnded))]
[JsonSerializable(typeof(ExtendStatus))]
[JsonSerializable(typeof(GpuInfo[]))]
[JsonSerializable(typeof(UpdateInfo))]
[JsonSerializable(typeof(WhatsNewRelease[]))]
[JsonSerializable(typeof(LatencyStats))]
[JsonSerializable(typeof(PeerInfo))]
[JsonSerializable(typeof(DisplayInfo))]
[JsonSerializable(typeof(TrustedPeer[]))]
[JsonSerializable(typeof(MonitorInfo[]))]
[JsonSerializable(typeof(StreamWrapper))]
[JsonSerializable(typeof(CoreEvent.SessionEnded))]
[JsonSerializable(typeof(CoreEvent.KeyframeRequested))]
[JsonSerializable(typeof(CoreEvent.BitrateHint))]
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
