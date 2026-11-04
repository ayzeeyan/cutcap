using System.Diagnostics;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Media;
using Avalonia.Media.Imaging;
using Avalonia.Platform;
using CutCap.Interop;

namespace CutCap.Desktop.Controls;

/// <summary>
/// Presents decoded source frames. Pixels are copied once, by the engine,
/// straight into a reused <see cref="WriteableBitmap"/> (RGBA8, no conversion);
/// frames never pass through view-model property notifications. The picture is
/// fitted to the panel with its real aspect ratio.
/// </summary>
public sealed class FrameView : Control
{
    private static readonly bool TracePlayback = Environment.GetEnvironmentVariable("CUTCAP_TRACE_PLAYBACK") == "1";

    /// <summary>Opt-in diagnostic: DrawImage submitted this generation, not physical scanout.</summary>
    public event Action<ulong, long>? FrameRendered;
    private WriteableBitmap? _bitmap;
    private ulong _generation;

    /// <summary>Microseconds of the last engine → bitmap copy (diagnostics).</summary>
    public double LastCopyMicroseconds { get; private set; }

    public bool HasFrame => _bitmap is not null;

    public ulong Generation => _generation;

    /// <summary>Copies the engine's current frame if it is newer than the one shown.</summary>
    public void Present(EngineSession session)
    {
        if (!session.TryGetFrameInfo(out var info))
        {
            Clear();
            return;
        }
        if (_bitmap is not null && info.Generation == _generation)
        {
            return;
        }
        var size = new PixelSize((int)info.Width, (int)info.Height);
        if (_bitmap is null || _bitmap.PixelSize != size)
        {
            _bitmap?.Dispose();
            _bitmap = new WriteableBitmap(size, new Vector(96, 96), PixelFormats.Rgba8888, AlphaFormat.Opaque);
        }
        var watch = Stopwatch.StartNew();
        FrameCopyResult result;
        using (var target = _bitmap.Lock())
        {
            result = session.CopyFrame(info.Generation, target.Address, target.RowBytes * target.Size.Height, target.RowBytes);
        }
        LastCopyMicroseconds = watch.Elapsed.TotalMicroseconds;
        if (result == FrameCopyResult.Copied)
        {
            _generation = info.Generation;
            InvalidateVisual();
        }
        // Stale: a newer frame arrived between info and copy; the next pump presents it.
    }

    public void Clear()
    {
        if (_bitmap is null)
        {
            return;
        }
        _bitmap.Dispose();
        _bitmap = null;
        _generation = 0;
        InvalidateVisual();
    }

    public override void Render(DrawingContext context)
    {
        if (_bitmap is null)
        {
            return;
        }
        var source = new Rect(0, 0, _bitmap.PixelSize.Width, _bitmap.PixelSize.Height);
        var scale = Math.Min(Bounds.Width / source.Width, Bounds.Height / source.Height);
        if (scale <= 0 || double.IsNaN(scale))
        {
            return;
        }
        var width = source.Width * scale;
        var height = source.Height * scale;
        var destination = new Rect((Bounds.Width - width) / 2, (Bounds.Height - height) / 2, width, height);
        context.DrawImage(_bitmap, source, destination);
        if (TracePlayback) FrameRendered?.Invoke(_generation, Stopwatch.GetTimestamp());
    }

    protected override void OnDetachedFromVisualTree(VisualTreeAttachmentEventArgs e)
    {
        base.OnDetachedFromVisualTree(e);
        Clear();
    }
}
