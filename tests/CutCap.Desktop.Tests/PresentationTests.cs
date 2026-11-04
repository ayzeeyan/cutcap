using System.Collections.ObjectModel;
using CutCap.Desktop.Controls;
using CutCap.Desktop.ViewModels;
using Xunit;

namespace CutCap.Desktop.Tests;

/// <summary>Frontend logic that must hold without rendering: view coordinates and list identity.</summary>
public sealed class PresentationTests
{
    [Fact]
    public void TimelineCoordinatesRoundTripAndClampZoom()
    {
        var scale = TimelineScale.ForZoom(100);
        Assert.Equal(20, scale.PixelsPerSecond);
        Assert.Equal(12, scale.TimeToX(0));
        Assert.Equal(1500, scale.XToTime(scale.TimeToX(1500)), 6);
        Assert.Equal(0, scale.XToTime(-500));
        Assert.Equal(10, TimelineScale.ForZoom(10).PixelsPerSecond);
        Assert.Equal(40, TimelineScale.ForZoom(900).PixelsPerSecond);
    }

    [Fact]
    public void RulerLabelsStayReadableAtEveryZoom()
    {
        for (var zoom = TimelineScale.MinZoom; zoom <= TimelineScale.MaxZoom; zoom += TimelineScale.ZoomStep)
        {
            var scale = TimelineScale.ForZoom(zoom);
            Assert.True(scale.LabelStepSeconds() * scale.PixelsPerSecond >= 56, $"zoom {zoom}");
        }
        Assert.Equal("01:05", TimelineScale.FormatSeconds(65));
    }

    [Fact]
    public void RulerDrawsOnlyTheVisibleRange()
    {
        var scale = TimelineScale.ForZoom(100);
        var (first, last) = scale.VisibleSeconds(left: 412, right: 812);
        Assert.Equal(20, first);
        Assert.Equal(40, last);
        Assert.True(scale.ContentWidth(300) >= scale.TimeToX(TimelineScale.MinimumViewSeconds * 1000));
        Assert.Equal(5000, scale.ContentWidth(5000));
    }

    private sealed record Item(string Id, int Version);

    [Fact]
    public void SyncKeepsItemIdentityAcrossReorderAndUpdate()
    {
        var target = new ObservableCollection<Item>();
        Sync.Items(target, ["a", "b", "c"], i => i.Id, s => s, s => new Item(s, 0), (_, _) => { });
        var b = target[1];
        var updated = new List<Item>();
        Sync.Items(target, ["c", "b"], i => i.Id, s => s, s => new Item(s, 1), (item, _) => updated.Add(item));
        Assert.Equal(["c", "b"], target.Select(i => i.Id));
        Assert.Same(b, target[1]);
        Assert.Equal(2, updated.Count);
    }

    [Theory]
    [InlineData(0, "00:00.000")]
    [InlineData(1500, "00:01.500")]
    [InlineData(4_500_250, "75:00.250")]
    [InlineData(-20, "00:00.000")]
    public void PreciseTimeMatchesTheEngineNotation(long ms, string expected)
    {
        Assert.Equal(expected, Sync.Precise(ms));
    }

    [Fact]
    public void EveryIconHasPathData()
    {
        Assert.True(IconData.Paths.Count >= 40);
        Assert.All(IconData.Paths, pair => Assert.False(pair.Value.Stroke.Length == 0 && pair.Value.Fill.Length == 0, pair.Key));
        foreach (var used in new[] { "Import", "Music", "Text", "Sticker", "Effects", "Transition", "Filters", "Sliders", "Film", "Play" })
        {
            Assert.True(IconData.Paths.ContainsKey(used), used);
        }
    }
}
