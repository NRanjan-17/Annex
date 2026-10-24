using System.Security.Cryptography;
using Windows.Networking.Connectivity;
using Windows.Networking.NetworkOperators;

namespace Annex.App;

/// <summary>
/// A direct Wi-Fi link through Windows' Mobile hotspot: the other computer joins this PC's own
/// network, so video goes straight between them (one hop, no router). Each start uses a fresh
/// random network name and passphrase; the user's own hotspot settings are restored on stop.
/// </summary>
internal sealed class Hotspot
{
    /// <summary>Windows' hotspot always gives this PC this address.</summary>
    public const string HostAddress = "192.168.137.1";

    private NetworkOperatorTetheringManager? _manager;
    private (string Ssid, string Passphrase)? _previous;
    private Task<string?>? _starting;

    public string? Ssid { get; private set; }
    public string? Passphrase { get; private set; }
    public bool IsOn => Ssid is not null;

    /// <summary>Starts the hotspot. Returns null on success, or a message for the user.</summary>
    public Task<string?> StartAsync()
    {
        if (IsOn) return Task.FromResult<string?>(null);
        // A caller while a start is under way shares it: starting again would save the first
        // start's temporary network as the user's own, and theirs would never come back.
        if (_starting is { IsCompleted: false } running) return running;
        return _starting = StartCoreAsync();
    }

    private async Task<string?> StartCoreAsync()
    {
        try
        {
            var profile = NetworkInformation.GetInternetConnectionProfile()
                ?? NetworkInformation.GetConnectionProfiles()
                    .FirstOrDefault(p => p.GetNetworkConnectivityLevel() != NetworkConnectivityLevel.None);
            if (profile is null)
                return "Windows needs this PC to be on a network (Wi-Fi or Ethernet) to start a hotspot.";
            var capability = NetworkOperatorTetheringManager.GetTetheringCapabilityFromConnectionProfile(profile);
            if (capability != TetheringCapability.Enabled)
                return capability switch
                {
                    TetheringCapability.DisabledByGroupPolicy => "Mobile hotspot is turned off by your organisation's policy.",
                    TetheringCapability.DisabledByHardwareLimitation => "This PC's Wi-Fi adapter can't run a hotspot.",
                    _ => $"Windows can't start a hotspot on this PC right now ({capability}).",
                };
            _manager = NetworkOperatorTetheringManager.CreateFromConnectionProfile(profile);
            var config = _manager.GetCurrentAccessPointConfiguration();
            _previous = (config.Ssid, config.Passphrase);
            var ssid = "Annex-" + Convert.ToHexString(RandomNumberGenerator.GetBytes(2));
            var passphrase = RandomNumberGenerator.GetString("abcdefghijkmnpqrstuvwxyzACDEFGHJKLMNPQRSTUVWXYZ23456789", 20);
            config.Ssid = ssid;
            config.Passphrase = passphrase;
            await _manager.ConfigureAccessPointAsync(config).AsTask().ConfigureAwait(false);
            var result = await _manager.StartTetheringAsync().AsTask().ConfigureAwait(false);
            if (result.Status != TetheringOperationStatus.Success)
            {
                await RestoreAsync().ConfigureAwait(false);
                return $"Windows couldn't start the hotspot ({result.Status}{(string.IsNullOrEmpty(result.AdditionalErrorMessage) ? "" : ": " + result.AdditionalErrorMessage)}).";
            }
            Ssid = ssid;
            Passphrase = passphrase;
            return null;
        }
        catch (Exception e)
        {
            return $"Windows couldn't start the hotspot: {e.Message}";
        }
    }

    public async Task StopAsync()
    {
        if (_manager is null || !IsOn) return;
        try
        {
            await _manager.StopTetheringAsync().AsTask().ConfigureAwait(false);
        }
        catch (Exception)
        {
            // Already off.
        }
        await RestoreAsync().ConfigureAwait(false);
        Ssid = Passphrase = null;
    }

    /// <summary>Puts the user's own hotspot name and passphrase back.</summary>
    private async Task RestoreAsync()
    {
        if (_manager is null || _previous is not { } prev) return;
        try
        {
            var config = _manager.GetCurrentAccessPointConfiguration();
            config.Ssid = prev.Ssid;
            config.Passphrase = prev.Passphrase;
            await _manager.ConfigureAccessPointAsync(config).AsTask().ConfigureAwait(false);
        }
        catch (Exception)
        {
            // Best effort.
        }
        _previous = null;
    }
}
