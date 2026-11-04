using Avalonia;
using Avalonia.Controls;
using Avalonia.Controls.Documents;
using Avalonia.Media;

namespace CutCap.Desktop.Controls;

/// <summary>
/// Draws one authored 16×16 icon (Assets/Icons, generated into IconData) in the
/// inherited foreground. Stroke parts use a 1.5 px round pen, scaled with the icon.
/// </summary>
public sealed class IconView : Control
{
    public static readonly StyledProperty<string?> GlyphProperty =
        AvaloniaProperty.Register<IconView, string?>(nameof(Glyph));

    public static readonly StyledProperty<IBrush?> ForegroundProperty =
        TextElement.ForegroundProperty.AddOwner<IconView>();

    public static readonly StyledProperty<double> StrokeThicknessProperty =
        AvaloniaProperty.Register<IconView, double>(nameof(StrokeThickness), 1.5);

    private static readonly Dictionary<string, (Geometry? Stroke, Geometry? Fill)> Cache = new(StringComparer.Ordinal);

    static IconView()
    {
        AffectsRender<IconView>(GlyphProperty, ForegroundProperty, StrokeThicknessProperty);
    }

    public string? Glyph
    {
        get => GetValue(GlyphProperty);
        set => SetValue(GlyphProperty, value);
    }

    public IBrush? Foreground
    {
        get => GetValue(ForegroundProperty);
        set => SetValue(ForegroundProperty, value);
    }

    public double StrokeThickness
    {
        get => GetValue(StrokeThicknessProperty);
        set => SetValue(StrokeThicknessProperty, value);
    }

    /// <summary>Parses (once) the geometry for an icon name; null for unknown names.</summary>
    internal static (Geometry? Stroke, Geometry? Fill)? Resolve(string name)
    {
        if (Cache.TryGetValue(name, out var cached))
        {
            return cached;
        }
        if (!IconData.Paths.TryGetValue(name, out var data))
        {
            return null;
        }
        var parsed = (
            data.Stroke.Length > 0 ? Geometry.Parse(data.Stroke) : null,
            data.Fill.Length > 0 ? Geometry.Parse(data.Fill) : null);
        Cache[name] = parsed;
        return parsed;
    }

    public override void Render(DrawingContext context)
    {
        if (Glyph is not { } name || Resolve(name) is not { } geometry || Foreground is not { } brush)
        {
            return;
        }
        var size = Math.Min(Bounds.Width, Bounds.Height);
        if (size <= 0)
        {
            return;
        }
        var scale = size / 16;
        var offset = new Vector((Bounds.Width - size) / 2, (Bounds.Height - size) / 2);
        using (context.PushTransform(Matrix.CreateScale(scale, scale) * Matrix.CreateTranslation(offset)))
        {
            if (geometry.Stroke is { } stroke)
            {
                var pen = new Pen(brush, StrokeThickness, lineCap: PenLineCap.Round, lineJoin: PenLineJoin.Round);
                context.DrawGeometry(null, pen, stroke);
            }
            if (geometry.Fill is { } fill)
            {
                context.DrawGeometry(brush, null, fill);
            }
        }
    }
}
