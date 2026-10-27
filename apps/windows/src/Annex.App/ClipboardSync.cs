using System.Runtime.InteropServices;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Text;
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
    }

    public void Stop()
    {
        if (!_on) return;
        _on = false;
        Clipboard.ContentChanged -= OnChanged;
    }

    private async void OnChanged(object? sender, object e)
    {
        try
        {
            if (await ReadAsync() is var (mime, data)) Copied?.Invoke(mime, data);
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        {
            // Another app holds the clipboard.
        }
    }

    /// <summary>What's on the clipboard now, if it's text or a picture and not marked private.</summary>
    public async Task<(string Mime, byte[] Data)?> ReadNowAsync()
    {
        try { return await ReadAsync(); }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException) { return null; }
    }

    private static async Task<(string, byte[])?> ReadAsync()
    {
        var view = Clipboard.GetContent();
        if (view.Contains(FromAnnex) || view.AvailableFormats.Any(f => Secret.Contains(f))) return null;
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
        return null;
    }

    /// <summary>Puts what the other computer copied on this PC's clipboard, kept out of Windows'
    /// clipboard history and cloud clipboard.</summary>
    public async void Apply(string mime, byte[] data)
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
            return;
        }
        try
        {
            Clipboard.SetContentWithOptions(package, new ClipboardContentOptions { IsAllowedInHistory = false, IsRoamable = false });
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        {
            // Another app holds the clipboard.
        }
    }
}
