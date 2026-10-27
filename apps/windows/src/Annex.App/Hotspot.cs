using Annex.Interop;
using Windows.Networking.Connectivity;
using Windows.Networking.NetworkOperators;

namespace Annex.App;

/// <summary>
/// A direct Wi-Fi link through Windows' Mobile hotspot: the other computer joins this PC's own
/// network, so video goes straight between them (one hop, no router). It uses the hotspot exactly
/// as the user set it up in Settings → Network &amp; internet → Mobile hotspot, never changing its
/// name or password: the other computer remembers that network and joins it faster next time.
/// A hotspot the user had already turned on stays on afterwards.
/// </summary>
internal sealed class Hotspot
{
    /// <summary>Windows' hotspot always gives this PC this address.</summary>
    public const string HostAddress = "192.168.137.1";

    private NetworkOperatorTetheringManager? _manager;
    private bool _startedByAnnex;
    private Task<string?>? _starting;

    public string? Ssid { get; private set; }
    public string? Passphrase { get; private set; }
    public bool IsOn => Ssid is not null;

    /// <summary>Starts the hotspot. Returns null on success, or a message for the user.</summary>
    public Task<string?> StartAsync()
    {
        if (IsOn) return Task.FromResult<string?>(null);
        // A caller while a start is under way shares it.
        if (_starting is { IsCompleted: false } running) return running;
        return _starting = StartLoggedAsync();
    }

    private async Task<string?> StartLoggedAsync()
    {
        AnnexLog.Info("[network] starting the hotspot");
        var error = await StartCoreAsync().ConfigureAwait(false);
        if (error is null) AnnexLog.Info($"[network] hotspot on: \"{Ssid}\" ({(_startedByAnnex ? "started by Annex" : "was already on")})");
        else AnnexLog.Warn($"[network] hotspot didn't start: {error}");
        return error;
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
            if (string.IsNullOrEmpty(config.Ssid) || string.IsNullOrEmpty(config.Passphrase))
                return "Set up Mobile hotspot once in Windows Settings → Network & internet → Mobile hotspot, then try again.";
            _startedByAnnex = _manager.TetheringOperationalState != TetheringOperationalState.On;
            if (_startedByAnnex)
            {
                var result = await _manager.StartTetheringAsync().AsTask().ConfigureAwait(false);
                if (result.Status != TetheringOperationStatus.Success)
                {
                    _startedByAnnex = false;
                    return $"Windows couldn't start the hotspot ({result.Status}{(string.IsNullOrEmpty(result.AdditionalErrorMessage) ? "" : ": " + result.AdditionalErrorMessage)}).";
                }
            }
            Ssid = config.Ssid;
            Passphrase = config.Passphrase;
            return null;
        }
        catch (Exception e)
        {
            return $"Windows couldn't start the hotspot: {e.Message}";
        }
    }

    /// <summary>Turns the hotspot off again, unless the user had it on before Annex needed it.</summary>
    public async Task StopAsync()
    {
        if (_manager is null || !IsOn) return;
        if (_startedByAnnex)
        {
            try
            {
                var result = await _manager.StopTetheringAsync().AsTask().ConfigureAwait(false);
                AnnexLog.Info($"[network] hotspot off ({result.Status})");
            }
            catch (Exception e)
            {
                // Already off.
                AnnexLog.Debug($"[network] hotspot stop: {e.GetType().Name}: {e.Message}");
            }
        }
        else
        {
            AnnexLog.Info("[network] hotspot left on: it was on before Annex needed it");
        }
        _startedByAnnex = false;
        Ssid = Passphrase = null;
    }
}
