using Annex.Interop;

namespace Annex.Interop.Tests;

/// <summary>In-app updates through the P/Invoke layer: release choice and signature checks.</summary>
public class UpdateTests
{
    private static string Release(string tag, bool pre, bool signed) =>
        $$"""
        {"tag_name":"{{tag}}","draft":false,"prerelease":{{(pre ? "true" : "false")}},"body":"notes","html_url":"https://x/{{tag}}",
         "assets":[{"name":"Annex-Setup-{{tag[1..]}}-x64.exe","browser_download_url":"https://x/{{tag}}/exe"},
                   {"name":"SHA256SUMS.txt","browser_download_url":"https://x/{{tag}}/sums"}
                   {{(signed ? $$""",{"name":"SHA256SUMS.txt.sig","browser_download_url":"https://x/{{tag}}/sig"}""" : "")}}]}
        """;

    [NativeFact]
    public void PicksNormalOrBetaReleasesAndSkipsUnsignedOnes()
    {
        Assert.StartsWith("https://api.github.com/", AnnexNative.UpdateReleasesUrl);
        var json = $"[{Release("v99.0.1-pre.1", true, true)},{Release("v99.0.0", false, true)},{Release("v100.0.0", false, false)}]";

        var normal = AnnexNative.PickUpdate(json, beta: false, "-x64.exe");
        Assert.NotNull(normal);
        Assert.Equal("99.0.0", normal.Version);
        Assert.Equal("https://x/v99.0.0/exe", normal.AssetUrl);
        Assert.Equal("Annex-Setup-99.0.0-x64.exe", normal.AssetName);
        Assert.False(normal.Prerelease);

        var beta = AnnexNative.PickUpdate(json, beta: true, "-x64.exe");
        Assert.Equal("99.0.1-pre.1", beta?.Version);
        Assert.True(beta?.Prerelease);

        Assert.Null(AnnexNative.PickUpdate(json, beta: false, "-arm64.exe"));
        Assert.Throws<AnnexException>(() => AnnexNative.PickUpdate("not json", false, "-x64.exe"));
    }

    [NativeFact]
    public void RefusesFilesWithoutAGenuineSignature()
    {
        var dir = Directory.CreateTempSubdirectory("annex-update-test").FullName;
        try
        {
            var file = Path.Combine(dir, "Annex-Setup-99.0.0-x64.exe");
            File.WriteAllText(file, "installer");
            var sums = Path.Combine(dir, "SHA256SUMS.txt");
            File.WriteAllText(sums, $"{new string('0', 64)}  Annex-Setup-99.0.0-x64.exe\n");
            var sig = Path.Combine(dir, "SHA256SUMS.txt.sig");
            File.WriteAllBytes(sig, new byte[64]);
            var e = Assert.Throws<AnnexException>(() => AnnexNative.VerifyUpdate(sums, sig, file, "Annex-Setup-99.0.0-x64.exe"));
            Assert.Contains("signature", e.Message);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }
}
