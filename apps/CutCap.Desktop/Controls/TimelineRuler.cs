using System.Globalization;
using Avalonia;
using Avalonia.Automation.Peers;
using Avalonia.Controls;
using Avalonia.Media;

namespace CutCap.Desktop.Controls;

/// <summary>
/// Custom-drawn time ruler. Draws only the ticks inside the viewport, so cost
/// does not grow with the timeline length. Follows the lanes' horizontal offset.
/// </summary>
public sealed class TimelineRuler : Control
{
    public static readonly StyledProperty<int> ZoomProperty =
        AvaloniaProperty.Register<TimelineRuler, int>(nameof(Zoom), 100);

    public static readonly StyledProperty<double> OffsetProperty =
        AvaloniaProperty.Register<TimelineRuler, double>(nameof(Offset));

    private static readonly IBrush TickBrush = new SolidColorBrush(Color.Parse("#30343A"));
    private static readonly IBrush MajorBrush = new SolidColorBrush(Color.Parse("#868E96"));
    private static readonly IBrush PlayheadBrush = Brushes.White;
    private static readonly Typeface Mono = new("Cascadia Mono, Consolas");

    static TimelineRuler()
    {
        AffectsRender<TimelineRuler>(ZoomProperty, OffsetProperty);
    }

    public int Zoom
    {
        get => GetValue(ZoomProperty);
        set => SetValue(ZoomProperty, value);
    }

    public double Offset
    {
        get => GetValue(OffsetProperty);
        set => SetValue(OffsetProperty, value);
    }

    /// <summary>Named in the automation tree (a plain Control has no peer).</summary>
    protected override AutomationPeer OnCreateAutomationPeer() => new TimelineLanes.SurfacePeer(this);

    public override void Render(DrawingContext context)
    {
        var scale = TimelineScale.ForZoom(Zoom);
        var height = Bounds.Height;
        var step = scale.LabelStepSeconds();
        var (first, last) = scale.VisibleSeconds(Offset, Offset + Bounds.Width);
        using (context.PushClip(new Rect(Bounds.Size)))
        {
            context.FillRectangle(Brushes.Transparent, new Rect(Bounds.Size));
            for (var second = first; second <= last; second++)
            {
                var x = Math.Round(scale.TimeToX(second * 1000.0) - Offset) + 0.5;
                var major = second % step == 0;
                var tick = major ? 9 : 4;
                context.DrawLine(new Pen(major ? MajorBrush : TickBrush, 1), new Point(x, height - tick - 2), new Point(x, height - 2));
                if (major)
                {
                    var label = new FormattedText(TimelineScale.FormatSeconds(second), CultureInfo.InvariantCulture,
                        FlowDirection.LeftToRight, Mono, 10, MajorBrush);
                    context.DrawText(label, new Point(x + 8, 4));
                }
            }
            // Timeline playhead head at zero. It does not move until timeline playback exists.
            var head = Math.Round(scale.TimeToX(0) - Offset);
            if (head > -8 && head < Bounds.Width + 8)
            {
                context.DrawRectangle(PlayheadBrush, null, new Rect(head - 4.5, 3, 10, 13), 3, 3);
                context.DrawLine(new Pen(PlayheadBrush, 1), new Point(head + 0.5, 16), new Point(head + 0.5, height));
            }
        }
    }
}
