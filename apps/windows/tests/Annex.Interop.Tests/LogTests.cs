using Annex.Interop;

namespace Annex.Interop.Tests;

/// <summary>The app's log lines reach annex.log and the flight recorder through the P/Invoke layer.</summary>
public class LogTests
{
    [NativeFact]
    public void AppLinesReachTheLogAndTheRecording()
    {
        var dir = Directory.CreateTempSubdirectory("annex-log-test").FullName;
        try
        {
            // The log starts in the data folder of the first core opened with one.
            using (CoreClient.Open(dir, "C# Log"))
            {
                var id = Guid.NewGuid().ToString("N");
                AnnexLog.Info($"[test] info {id}");
                AnnexLog.Debug($"[test] debug {id}");
                AnnexLog.Trace($"[test] quiet trace {id}");
                AnnexLog.SetVerbose(true);
                Assert.True(AnnexLog.Verbose);
                AnnexLog.Trace($"[test] verbose trace {id}");
                AnnexLog.SetVerbose(false);

                var saved = AnnexLog.SaveRecording("csharp test");
                Assert.NotNull(saved);
                var recording = File.ReadAllText(saved);
                Assert.Contains($"[test] info {id}", recording);
                Assert.Contains($"[test] debug {id}", recording);
                Assert.Contains($"[test] verbose trace {id}", recording);
                Assert.DoesNotContain($"[test] quiet trace {id}", recording);

                var logDir = Path.GetDirectoryName(Path.GetDirectoryName(saved))!;
                using var log = new FileStream(Path.Combine(logDir, "annex.log"), FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
                Assert.Contains($"[test] info {id}", new StreamReader(log).ReadToEnd());
            }
        }
        finally
        {
            // The core keeps annex.log open for the rest of the process, so Windows may refuse this.
            try { Directory.Delete(dir, recursive: true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
        }
    }
}
