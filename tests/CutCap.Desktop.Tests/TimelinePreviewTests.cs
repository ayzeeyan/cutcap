using CutCap.Desktop.Services;
using CutCap.Desktop.ViewModels;
using CutCap.Interop;
using Xunit;

namespace CutCap.Desktop.Tests;

public sealed class TimelinePreviewTests
{
    private static readonly SourceView Source = new(true,"source.mkv",0,3000,false,false,true,"Source paused","","PTS 1000");
    private static readonly PlaybackView Paused = new(PlaybackState.Paused,false,"source-epoch",0,1000,1000,1000,null,null,4,4,4,0,0,2,null,null,"00:00.000","00:01.000","00:01.000",10);
    private static TimelinePreviewView Preview(long ticks=360_001) => new(
        ticks,new(1,720000),500,"00:00.500",$"{ticks} ticks",65000,3000,2160000,"00:03.000",
        "video1","Video 1","Timeline · Video 1 · single video track","ready","A.mkv · one video track",false,true,
        "clip-a","media-a","A.mkv",0,2160000,new(501,new(1,1000)),new(750,new(1,1000)),"PTS 750","model-epoch",1,100,120,null,1);
    [Fact]
    public void TimelineControlsUseEnginePositionAndPreserveTypedTextAcrossFrameUpdates()
    {
        using var host=new EngineHost(); var player=new PlayerViewModel(host); var timeline=Preview();
        player.ApplyMonitor(Source,Paused,PreviewMode.Timeline,timeline,true,true);
        Assert.False(player.CanPlay); Assert.False(player.CanStop); Assert.True(player.HasFrame);
        Assert.Equal("00:00.500",player.PositionText); Assert.Contains("Timeline",player.PositionName);
        Assert.Contains("Video 1",player.Title); Assert.Equal("360001 ticks",player.GoToText);
        player.GoToText="00:02.000";
        player.ApplyMonitor(Source,Paused,PreviewMode.Timeline,timeline with { FramesCopied=2 },true,true);
        Assert.Equal("00:02.000",player.GoToText);
        player.ApplyMonitor(Source,Paused,PreviewMode.Source,timeline,true,true);
        Assert.True(player.CanPlay); Assert.False(player.ShowPause); Assert.Equal(0,player.PreviewTabIndex);
        Assert.Equal("00:01.000",player.PositionText); Assert.Equal("Go to source time",player.GoToName);
    }
    [Fact]
    public void TimelinePositionDoesNotRebuildGeometryOrFollowSourcePlayback()
    {
        using var host=new EngineHost(); var timeline=new TimelineViewModel(host);
        var lanes=timeline.Lanes; var clips=timeline.Clips;
        timeline.ApplyPreview(Preview());
        Assert.Equal(500,timeline.PlayheadMs); Assert.Equal("00:00.500",timeline.PlayheadText);
        timeline.ApplyPreview(Preview() with { State="cancelled",HasFrame=false });
        Assert.Same(lanes,timeline.Lanes); Assert.Same(clips,timeline.Clips);
        Assert.Equal(500,timeline.PlayheadMs); // source mode keeps the independent position
    }
}
