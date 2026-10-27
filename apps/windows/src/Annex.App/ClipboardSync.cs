using System.Runtime.InteropServices;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Text;
using Annex.Interop;
using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace Annex.App;

/// <summary>Watches this PC's clipboard and writes what the other computer copied. Runs on the UI thread.</summary>
internal sealed class ClipboardSync
{
    public const string Text = "text/plain;charset=utf-8";
    public const string Png = "image/png";

    /// <summary>Marks what Annex itself put on the clipboard, so it isn't sent straight back.</summary>
    private const string FromAnnex = "Annex.FromOtherComputer";

    /// <summary>Password managers mark what they copy with these; it never leaves this PC.</summary>
    private static readonly string[] Secret = ["ExcludeClipboardContentFromMonitorProcessing", "Clipboard Viewer Ignore"];

    /// <summary>Something was copied on this PC.</summary>
    public event Action<string, byte[]>? Copied;

    private bool _on;

    public void Start()
    {
        if (_on) return;
        _on = true;
        Clipboard.ContentChanged += OnChanged;
        AnnexLog.Info("[clipboard] watching this PC's clipboard");
    }

    public void Stop()
    {
        if (!_on) return;
        _on = false;
        Clipboard.ContentChanged -= OnChanged;
        AnnexLog.Info("[clipboard] stopped watching this PC's clipboard");
    }

    private async void OnChanged(object? sender, object e)
    {
        try
        {
            if (await ReadAsync("copied on this PC") is var (mime, data)) Copied?.Invoke(mime, data);
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        {
            // Another app holds the clipboard.
            AnnexLog.Warn($"[clipboard] couldn't read a copy on this PC, another app holds the clipboard ({ex.GetType().Name} 0x{ex.HResult:x8})");
        }
    }

    /// <summary>What's on the clipboard now, if it's text or a picture and not marked private.</summary>
    public async Task<(string Mime, byte[] Data)?> ReadNowAsync(string why)
    {
        try { return await ReadAsync(why); }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        {
            AnnexLog.Warn($"[clipboard] couldn't read the clipboard ({why}), another app holds it ({ex.GetType().Name} 0x{ex.HResult:x8})");
            return null;
        }
    }

    /// <summary>Logs what was found (its kind and size, never what it says), or why it stays here.</summary>
    private static async Task<(string, byte[])?> ReadAsync(string why)
    {
        var result = await ReadCoreAsync(why);
        if (result is var (mime, data)) AnnexLog.Debug($"[clipboard] {why}: {mime}, {data.Length} bytes");
        return result;
    }

    private static async Task<(string, byte[])?> ReadCoreAsync(string why)
    {
        var view = Clipboard.GetContent();
        if (view.Contains(FromAnnex))
        {
            AnnexLog.Debug($"[clipboard] {why}: skipped, it came from the other computer");
            return null;
        }
        if (view.AvailableFormats.Any(f => Secret.Contains(f)))
        {
            AnnexLog.Debug($"[clipboard] {why}: skipped, marked private (password manager)");
            return null;
        }
        // A copied file also offers its icon; send its name, not the icon.
        if (view.Contains(StandardDataFormats.Bitmap) && !view.Contains(StandardDataFormats.StorageItems))
        {
            using var src = await (await view.GetBitmapAsync()).OpenReadAsync();
            var decoder = await BitmapDecoder.CreateAsync(src);
            using var bitmap = await decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Straight);
            using var png = new InMemoryRandomAccessStream();
            var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, png);
            encoder.SetSoftwareBitmap(bitmap);
            await encoder.FlushAsync();
            var bytes = new byte[png.Size];
            await png.GetInputStreamAt(0).ReadAsync(bytes.AsBuffer(), (uint)bytes.Length, InputStreamOptions.None);
            return (Png, bytes);
        }
        if (view.Contains(StandardDataFormats.Text) && await view.GetTextAsync() is { Length: > 0 } text)
            return (Text, Encoding.UTF8.GetBytes(text));
        AnnexLog.Debug($"[clipboard] {why}: skipped, not text or a picture (formats: {string.Join(", ", view.AvailableFormats.Take(8))})");
        return null;
    }

    /// <summary>Puts what the other computer copied on this PC's clipboard, kept out of Windows'
    /// clipboard history and cloud clipboard.</summary>
    public async void Apply(string mime, byte[] data)
    {
        try
        {
            var package = new DataPackage();
            package.SetData(FromAnnex, "1");
            if (mime.StartsWith("text/plain", StringComparison.Ordinal))
            {
                package.SetText(Encoding.UTF8.GetString(data));
            }
            else if (mime == Png)
            {
                var stream = new InMemoryRandomAccessStream();
                await stream.WriteAsync(data.AsBuffer());
                stream.Seek(0);
                package.SetBitmap(RandomAccessStreamReference.CreateFromStream(stream));
            }
            else
            {
                AnnexLog.Warn($"[clipboard] received {mime}, {data.Length} bytes: not a kind this PC can paste, skipped");
                return;
            }
            Clipboard.SetContentWithOptions(package, new ClipboardContentOptions { IsAllowedInHistory = false, IsRoamable = false });
            AnnexLog.Debug($"[clipboard] received and applied {mime}, {data.Length} bytes");
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        {
            // Another app holds the clipboard.
            AnnexLog.Warn($"[clipboard] couldn't apply what the other computer copied, another app holds the clipboard ({ex.GetType().Name} 0x{ex.HResult:x8})");
        }
        catch (Exception ex)
        {
            // async void: anything else would end Annex.
            AnnexLog.Error($"[clipboard] couldn't apply {mime}, {data.Length} bytes: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
