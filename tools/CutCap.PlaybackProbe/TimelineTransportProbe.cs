using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Nodes;
using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Media;
using Avalonia.Threading;
using Avalonia.VisualTree;
using CutCap.Desktop;
using CutCap.Desktop.Controls;
using CutCap.Desktop.ViewModels;
using CutCap.Interop;
using VideoFrame = CutCap.Desktop.Controls.FrameView;

/// <summary>Opt-in driver of the existing editor, clock, pump and bitmap. No alternative player.</summary>
internal sealed class TimelineTransportProbe
{
    private readonly string _sources, _output;
    private readonly Queue<(string Name,int Run,bool Large)> _plan = new(
        from name in new[]{"720p","1080p","4k","vfr","large"}
        from run in Enumerable.Range(1,3) select (name,run,name=="large"));
    private readonly DispatcherTimer _timer;
    private readonly Stopwatch _phase=Stopwatch.StartNew();
    private readonly List<object> _results=[];
    private readonly List<RenderSample> _renders=[];
    private readonly List<object> _transitions=[];
    private readonly List<double> _dispatchGaps=[];
    private List<object> _seeks=[];
    private readonly Queue<long> _seekPlan=new();
    private readonly Process _host=Process.GetCurrentProcess();
    private readonly Dictionary<int,Process> _children=[];
    private readonly Dictionary<int,double> _childCpu=[];
    private EngineSession? _session;
    private VideoFrame? _frame;
    private TimelineLanes? _lanes;
    private (string Name,int Run,bool Large) _item;
    private string _stage="new",_previousProject="";
    private string? _error;
    private PlaybackState? _state;
    private ulong _generation,_timelineReads,_timelineGeneration;
    private long _started,_lastTick,_lastResource,_seekStarted,_seekTarget;
    private bool _seekDone;
    private double _hostCpu;
    private long _peakWorking,_peakPrivate,_peakHost,_peakChildren;
    private int _peakView,_timelineBytes,_peakChildCount;
    private Task<string>? _fixture;
    private sealed record RenderSample(ulong Generation,double WallMs,long ClockTicks,long? FrameTicks,string? ClipId,SourceTimestamp? Pts,double CopyUs,double TimelineDrawUs);

    private TimelineTransportProbe(string sources,string output)
    {
        _sources=sources; _output=output;
        _timer=new DispatcherTimer(TimeSpan.FromMilliseconds(16),DispatcherPriority.Background,(_,_)=>Tick());
    }
    public static int Run(string[] args)
    {
        if(args.Length!=2) throw new ArgumentException("--timeline-play <generated-source-directory> <report.json>");
        Environment.SetEnvironmentVariable("CUTCAP_TRACE_PLAYBACK","1");
        Environment.SetEnvironmentVariable("CUTCAP_TRACE_TIMELINE","1");
        var probe=new TimelineTransportProbe(Path.GetFullPath(args[0]),Path.GetFullPath(args[1]));
        AppBuilder.Configure<App>().UsePlatformDetect().WithInterFont().With(new FontManagerOptions{DefaultFamilyName="fonts:Inter#Inter"})
            .AfterSetup(_=>probe._timer.Start()).StartWithClassicDesktopLifetime([]);
        probe._timer.Stop();
        if(probe._frame is not null) probe._frame.FrameRendered-=probe.Rendered;
        foreach(var process in probe._children.Values) process.Dispose();
        if(probe._error is null && probe._results.Count!=15) probe._error="Closed before 15 episodes completed";
        File.WriteAllText(probe._output,JsonSerializer.Serialize(new{date=DateTimeOffset.UtcNow,error=probe._error,
            method="Existing Avalonia editor/EngineHost/FrameView, distinct DrawImage generations, not physical scanout. Rust continuous timeline. Host plus observed FFmpeg/FFprobe resources sampled every 100ms, child CPU is observed lower bound. One isolated run, no other FFmpeg at start. Three episodes per source/project; 5000-clip episodes run first 10 timeline seconds.",
            runs=probe._results},new JsonSerializerOptions{WriteIndented=true}));
        probe._host.Dispose(); return probe._error is null?0:1;
    }
    private string Source => _item.Name=="vfr" ? Path.GetFullPath(Path.Combine(_sources,"../../../tests/fixtures/colors-vfr.mkv")) : Path.Combine(_sources,(_item.Large?"720p":_item.Name)+".mp4");
    private string ProjectFile => Path.Combine(Path.GetDirectoryName(_output)!,"transport-template.cutcap");
    private void Stage(string value) { _stage=value; _phase.Restart(); }
    private void Rendered(ulong generation,long timestamp)
    {
        if(generation==_generation || _session?.LastReadView is not {PreviewMode:PreviewMode.Timeline,Frame:{} f,TimelinePlayback:{} t} view || f.Generation!=generation) return;
        _generation=generation;
        if(_stage=="seek")
        {
            if(!_seekDone && t.State==PlaybackState.Paused && view.TimelinePreview?.PositionMs==_seekTarget)
            { _seeks.Add(new{targetMs=_seekTarget,renderLatencyMs=Stopwatch.GetElapsedTime(_seekStarted,timestamp).TotalMilliseconds,f.Actual,t.RequestToReadyUs}); _seekDone=true; }
        }
        else if(_stage=="play" && t.State is PlaybackState.Playing or PlaybackState.Buffering)
            _renders.Add(new(generation,Stopwatch.GetElapsedTime(_started,timestamp).TotalMilliseconds,t.ClockTicks,t.OfferedTicks,
                view.TimelinePreview?.ClipId,f.Actual,_frame!.LastCopyMicroseconds,_lanes!.LastDrawMicroseconds));
    }
    private void Resources()
    {
        if(Stopwatch.GetElapsedTime(_lastResource).TotalMilliseconds<100) return;
        _lastResource=Stopwatch.GetTimestamp(); _host.Refresh();
        long working=_host.WorkingSet64,commit=_host.PrivateMemorySize64,childrenWorking=0; var count=0;
        foreach(var child in Process.GetProcessesByName("ffmpeg").Concat(Process.GetProcessesByName("ffprobe")))
        {
            if(!_children.TryAdd(child.Id,child)) child.Dispose();
        }
        foreach(var (id,p) in _children)
        {
            try { _childCpu[id]=p.TotalProcessorTime.TotalMilliseconds; p.Refresh(); if(!p.HasExited) { count++; childrenWorking+=p.WorkingSet64; commit+=p.PrivateMemorySize64; } }
            catch(Exception e) when(e is InvalidOperationException or System.ComponentModel.Win32Exception) { }
        }
        _peakHost=Math.Max(_peakHost,working); _peakChildren=Math.Max(_peakChildren,childrenWorking);
        _peakWorking=Math.Max(_peakWorking,working+childrenWorking); _peakPrivate=Math.Max(_peakPrivate,commit); _peakChildCount=Math.Max(_peakChildCount,count);
    }
    private void BeginPlay(SessionView view)
    {
        _renders.Clear(); _transitions.Clear(); _dispatchGaps.Clear(); _seeks=[]; _state=null; _generation=0;
        foreach(var p in _children.Values) p.Dispose(); _children.Clear(); _childCpu.Clear();
        _peakWorking=0; _peakPrivate=0; _peakHost=0; _peakChildren=0; _peakView=0; _peakChildCount=0;
        _timelineReads=_session!.TimelineReadCount; _timelineGeneration=view.TimelineGeneration; _timelineBytes=_session.LastTimelineBytes;
        _hostCpu=_host.TotalProcessorTime.TotalMilliseconds; _lastTick=0; _lastResource=0;
        _started=Stopwatch.GetTimestamp(); Stage("play"); _session.PlayTimeline();
    }
    private static string LargeFixture(string template,string destination)
    {
        // Test data construction on a worker, never production clip evaluation.
        var doc=JsonNode.Parse(File.ReadAllText(template))!; var first=doc["project"]!["clips"]![0]!;
        var clips=new JsonArray();
        for(var i=0;i<5000;i++) { var c=first.DeepClone(); c["id"]=Guid.NewGuid().ToString(); c["start"]=i*720000L; c["duration"]=720000L; clips.Add(c); }
        doc["project"]!["clips"]=clips; doc["project"]!["name"]="5000 clip playback measurement";
        File.WriteAllText(destination,doc.ToJsonString()); return destination;
    }
    private void Tick()
    {
        try
        {
            var desktop=(IClassicDesktopStyleApplicationLifetime)Application.Current!.ApplicationLifetime!;
            if(desktop.MainWindow?.DataContext is not MainWindowViewModel model || !model.EngineReady) return;
            _session=model.Player.Session??throw new InvalidOperationException("No session"); var view=_session.LastReadView; if(view is null) return;
            if(_frame is null) { _frame=desktop.MainWindow.GetVisualDescendants().OfType<VideoFrame>().Single(); _frame.FrameRendered+=Rendered; _lanes=desktop.MainWindow.GetVisualDescendants().OfType<TimelineLanes>().Single(); }
            if(_stage=="new")
            {
                if(!_plan.TryDequeue(out _item)) { Finish(desktop); return; }
                _previousProject=view.Project?.Id??""; _session.SetPreviewMode(PreviewMode.Source); _session.NewProject(); Stage("empty");
            }
            else if(_stage=="empty" && view.Project?.Id!=_previousProject && view.Media.Count==0) { _session.Import(Source); Stage("import"); }
            else if(_stage=="import" && view.Media.Count==1 && !view.Import.Busy && view.Frame is not null) { _session.InsertClip(view.Media[0].Id,view.Tracks[0].Id); Stage("clip-a"); }
            else if(_stage=="clip-a" && view.Project?.ClipCount==1) { _session.InsertClip(view.Media[0].Id,view.Tracks[0].Id); Stage("clip-b"); }
            else if(_stage=="clip-b" && view.Project?.ClipCount==2 && view.Pending==0)
            {
                if(_item.Large) { _session.SaveProjectAs(ProjectFile); Stage("save"); }
                else { _session.SetPreviewMode(PreviewMode.Timeline); Stage("monitor"); }
            }
            else if(_stage=="save" && view.Project?.Document?.State=="saved" && view.Pending==0)
            { _fixture=Task.Run(()=>LargeFixture(ProjectFile,Path.Combine(Path.GetDirectoryName(_output)!,"transport-5000.cutcap"))); Stage("build"); }
            else if(_stage=="build" && _fixture?.IsCompleted==true) { _session.OpenProject(_fixture.GetAwaiter().GetResult()); Stage("open"); }
            else if(_stage=="open" && view.Project?.ClipCount==5000 && view.Pending==0 && view.Media.All(m=>m.Availability=="ready")) { _session.SetPreviewMode(PreviewMode.Timeline); Stage("monitor"); }
            else if(_stage=="monitor" && view.TimelinePreview is {Busy:false,PositionMs:0}) BeginPlay(view);
            else if(_stage=="play")
            {
                var t=view.TimelinePlayback??throw new InvalidOperationException("No timeline transport");
                if(t.State==PlaybackState.Failed) throw new InvalidOperationException(t.Error);
                var now=Stopwatch.GetTimestamp(); if(_lastTick!=0) _dispatchGaps.Add(Stopwatch.GetElapsedTime(_lastTick,now).TotalMilliseconds); _lastTick=now;
                Resources(); _peakView=Math.Max(_peakView,_session.LastViewBytes);
                if(_state!=t.State) { _state=t.State; _transitions.Add(new{state=t.State.ToString(),wallMs=Stopwatch.GetElapsedTime(_started).TotalMilliseconds,t.ClockTicks,t.BufferStalls}); }
                if(view.TimelineGeneration!=_timelineGeneration || _session.TimelineReadCount!=_timelineReads) throw new InvalidOperationException("Playback rebuilt timeline document");
                if(t.State==PlaybackState.Ended || _item.Large && t.ClockMs>=10000)
                {
                    Resources(); var elapsed=Stopwatch.GetElapsedTime(_started).TotalSeconds;
                    var cpu=_host.TotalProcessorTime.TotalMilliseconds-_hostCpu+_childCpu.Values.Sum();
                    _results.Add(new{source=_item.Name,run=_item.Run,clipCount=view.Project!.ClipCount,seconds=elapsed,rendered=_renders.Count,distinctRenderFps=_renders.Count/elapsed,
                        t.Decoded,t.Offered,t.Copied,t.Dropped,t.BufferStalls,t.Handoffs,t.DecoderStarts,t.RequestToReadyUs,
                        peakHostAndChildrenWorkingSetBytes=_peakWorking,peakHostAndChildrenPrivateBytes=_peakPrivate,peakHostWorkingSetBytes=_peakHost,peakChildWorkingSetBytes=_peakChildren,peakObservedChildren=_peakChildCount,
                        cpuObservedOneCorePercent=cpu/(elapsed*1000)*100,cpuObservedMachinePercent=cpu/(elapsed*1000*Environment.ProcessorCount)*100,
                        scale=desktop.MainWindow.RenderScaling,client=desktop.MainWindow.ClientSize,peakViewBytes=_peakView,timelineBytes=_timelineBytes,timelineReadsDuringPlay=_session.TimelineReadCount-_timelineReads,
                        transitions=_transitions.ToArray(),frames=_renders.ToArray(),dispatcherGapsMs=_dispatchGaps.ToArray(),seeks=_seeks});
                    foreach(var target in new long[]{250,1250,2750}) _seekPlan.Enqueue(target);
                    _session.PauseTimeline(); _seekDone=true; Stage("seek");
                    Console.WriteLine($"Timeline {_item.Name} {_item.Run}: {_renders.Count} DrawImage generations / {elapsed:F3}s; stalls {t.BufferStalls}");
                }
            }
            else if(_stage=="seek" && _seekDone)
            {
                if(_seekPlan.TryDequeue(out _seekTarget)) { _seekDone=false; _seekStarted=Stopwatch.GetTimestamp(); _session.SeekTimelineMilliseconds(_seekTarget); _phase.Restart(); }
                else { _session.StopTimeline(); Stage("new"); }
            }
            if(view.Import.Error) throw new InvalidOperationException(view.Import.Message);
            if(_phase.Elapsed>TimeSpan.FromSeconds(35)) throw new TimeoutException($"{_stage}: {view.TimelinePlayback} / {view.Status}");
        }
        catch(Exception e) { _error=e.ToString(); Console.Error.WriteLine(e); if(Application.Current?.ApplicationLifetime is IClassicDesktopStyleApplicationLifetime d) Finish(d); }
    }
    private void Finish(IClassicDesktopStyleApplicationLifetime desktop)
    {
        if(desktop.MainWindow?.DataContext is MainWindowViewModel model) model.ConfirmUnsaved=()=>Task.FromResult(UnsavedChoice.Discard);
        _timer.Stop(); desktop.Shutdown();
    }
}
