namespace Annex.Interop.Tests;

/// <summary>A test that needs the native annex_ffi library (set ANNEX_NATIVE_DIR).</summary>
public sealed class NativeFactAttribute : FactAttribute
{
    public NativeFactAttribute()
    {
        if (string.IsNullOrEmpty(Environment.GetEnvironmentVariable("ANNEX_NATIVE_DIR")))
            Skip = "ANNEX_NATIVE_DIR not set; build core (cargo build -p annex-ffi) and point it at the output folder.";
    }
}
