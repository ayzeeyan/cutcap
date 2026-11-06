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

/// <summary>Opt-in measurements in the actual editor, with its original host and bitmap presenter.</summary>
internal sealed class TimelineProbe
{
    private readonly string _directory;
    private readonly List<object> _samples=[];
    private readonly Queue<long> _positions=new();
    private readonly DispatcherTimer _timer;
    private readonly Stopwatch _lifetime=Stopwatch.StartNew();
    private readonly Stopwatch _stageWatch=Stopwatch.StartNew();
    private readonly Process _host=Process.GetCurrentProcess();
    private EngineSession? _session;
    private VideoFrame? _frame;
    private TimelineLanes? _lanes;
    private Task<string>? _largeFixture;
    private string _stage="start",_project="small";
    private long _target,_requested,_lastTick;
    private bool _rendered;
    private ulong _oldGeneration,_startTimelineGeneration,_startTimelineReads;
    private int _peakViewBytes,_timelineBytes;
    private long _peakHostWorking,_peakHostPrivate,_peakChildrenWorking;
    private double _maxDispatcherGap;
    private string? _error;

    private TimelineProbe(string directory)
    {
        _directory=directory;
        _timer=new DispatcherTimer(TimeSpan.FromMilliseconds(16),DispatcherPriority.Background,(_,_)=>Tick());
    }
    public static int Run(string[] args)
    {
        if(args.Length!=2) throw new ArgumentException("--timeline <phase3c-directory containing media> <report.json>");
        Environment.SetEnvironmentVariable("CUTCAP_TRACE_PLAYBACK","1");
        Environment.SetEnvironmentVariable("CUTCAP_TRACE_TIMELINE","1");
        var probe=new TimelineProbe(Path.GetFullPath(args[0]));
        AppBuilder.Configure<App>().UsePlatformDetect().WithInterFont()
            .With(new FontManagerOptions {DefaultFamilyName="fonts:Inter#Inter"})
            .AfterSetup(_=>probe._timer.Start()).StartWithClassicDesktopLifetime([]);
        probe._timer.Stop();
        if(probe._frame is not null) probe._frame.FrameRendered-=probe.Rendered;
        File.WriteAllText(args[1],JsonSerializer.Serialize(new {
            date=DateTimeOffset.UtcNow, error=probe._error,
            method="Existing CutCap App/EngineHost/FrameView. Distinct DrawImage callbacks, not physical scanout. Source decoding is the unchanged isolated FFmpeg subprocess route. Process names sampled only in this isolated benchmark; no other FFmpeg was running at start.",
            samples=probe._samples, peakViewBytes=probe._peakViewBytes, largeTimelineBytes=probe._timelineBytes,
            peakHostWorkingSetBytes=probe._peakHostWorking, peakHostPrivateBytes=probe._peakHostPrivate,
            peakObservedFfmpegWorkingSetBytes=probe._peakChildrenWorking, maxDiagnosticDispatcherGapMs=probe._maxDispatcherGap,
            seconds=probe._lifetime.Elapsed.TotalSeconds
        },new JsonSerializerOptions {WriteIndented=true}));
        probe._host.Dispose();
        return probe._error is null?0:1;
    }
    private static string BuildLarge(string template,string destination)
    {
        // Fixture construction only, on a worker; production clip semantics stay in Rust.
        var document=JsonNode.Parse(File.ReadAllText(template))!;
        var first=document["project"]!["clips"]![0]!;
        var clips=new JsonArray();
        for(var i=0;i<5000;i++)
        {
            var clip=first.DeepClone(); clip["id"]=Guid.NewGuid().ToString();
            clip["start"]=i*720_000L; clip["duration"]=720_000L; clips.Add(clip);
        }
        document["project"]!["clips"]=clips; document["project"]!["name"]="Large timeline measurement";
        File.WriteAllText(destination,document.ToJsonString()); return destination;
    }
    private void Rendered(ulong generation,long timestamp)
    {
        if(_stage!="seek"||_rendered||generation==_oldGeneration) return;
        var view=_session?.LastReadView;
        if(view?.Frame?.Generation!=generation||view.TimelinePreview is not {State:"ready"} t||t.PositionMs!=_target) return;
        _rendered=true;
        _samples.Add(new {project=_project,targetMs=_target,renderLatencyMs=Stopwatch.GetElapsedTime(_requested,timestamp).TotalMilliseconds,
            t.EvaluationUs,t.DecodeUs,t.RequestToReadyUs,t.ProcessId,t.Actual,viewBytes=_session!.LastViewBytes,
            copyUs=_frame!.LastCopyMicroseconds,timelineDrawUs=_lanes!.LastDrawMicroseconds,
            hostWorkingSetBytes=_host.WorkingSet64,hostPrivateBytes=_host.PrivateMemorySize64,
            timelineDrawCount=_lanes.DiagnosticRenderCount,timelineGeneration=view.TimelineGeneration,
            timelineReadsSincePlan=_session.TimelineReadCount-_startTimelineReads});
    }
    private void Plan(SessionView view,bool large=false)
    {
        _project=large?"5000-clips":"2-clips";
        _startTimelineGeneration=view.TimelineGeneration; _startTimelineReads=_session!.TimelineReadCount;
        if(large) for(var i=0;i<30;i++) _positions.Enqueue((i*167L%5000)*1000+250);
        else for(var i=0;i<36;i++) _positions.Enqueue(new long[]{500,3500,1000,4000,1500,4500}[i%6]);
        _positions.Enqueue(large?5_001_000:6000);
        Next();
    }
    private void Next()
    {
        if(_positions.TryDequeue(out _target))
        {
            _oldGeneration=_session!.LastReadView?.Frame?.Generation??0;
            _requested=Stopwatch.GetTimestamp(); _rendered=false; _stage="seek"; _stageWatch.Restart();
            _session.SeekTimelineMilliseconds(_target);
        }
        else { _stage=_project=="2-clips"?"open-large":"rapid"; _stageWatch.Restart(); }
    }
    private void Tick()
    {
        try
        {
            var desktop=(IClassicDesktopStyleApplicationLifetime)Application.Current!.ApplicationLifetime!;
            if(desktop.MainWindow?.DataContext is not MainWindowViewModel model||!model.EngineReady) return;
            _session=model.Player.Session??throw new InvalidOperationException("No engine session");
            var view=_session.LastReadView; if(view is null) return;
            var now=Stopwatch.GetTimestamp();
            if(_lastTick!=0) _maxDispatcherGap=Math.Max(_maxDispatcherGap,Stopwatch.GetElapsedTime(_lastTick,now).TotalMilliseconds);
            _lastTick=now;
            if(_frame is null)
            {
                _frame=desktop.MainWindow.GetVisualDescendants().OfType<VideoFrame>().Single();
                _lanes=desktop.MainWindow.GetVisualDescendants().OfType<TimelineLanes>().Single();
                _frame.FrameRendered+=Rendered;
            }
            _peakViewBytes=Math.Max(_peakViewBytes,_session.LastViewBytes);
            _host.Refresh(); _peakHostWorking=Math.Max(_peakHostWorking,_host.WorkingSet64); _peakHostPrivate=Math.Max(_peakHostPrivate,_host.PrivateMemorySize64);
            long children=0;
            foreach(var child in Process.GetProcessesByName("ffmpeg"))
            {
                using(child) { try { children+=child.WorkingSet64; } catch(InvalidOperationException) {} }
            }
            _peakChildrenWorking=Math.Max(_peakChildrenWorking,children);
            if(_stage=="start")
            {
                if(Environment.GetEnvironmentVariable("CUTCAP_PROBE_MIN_LAYOUT")=="1")
                { desktop.MainWindow.Width=desktop.MainWindow.MinWidth; desktop.MainWindow.Height=desktop.MainWindow.MinHeight; }
                _session.Import(Path.Combine(_directory,"media","A red.mkv")); _stage="import-a";
            }
            else if(_stage=="import-a"&&view.Media.Count==1&&!view.Import.Busy&&view.Frame is not null)
            { _session.InsertClip(view.Media[0].Id,view.Tracks[0].Id); _stage="clip-a"; }
            else if(_stage=="clip-a"&&view.Project?.ClipCount==1)
            { _session.Import(Path.Combine(_directory,"media","B blue.mkv")); _stage="import-b"; }
            else if(_stage=="import-b"&&view.Media.Count==2&&!view.Import.Busy&&view.Source.Title=="B blue.mkv"&&!view.Source.Busy)
            { _session.InsertClip(view.Media[1].Id,view.Tracks[0].Id); _stage="clip-b"; }
            else if(_stage=="clip-b"&&view.Project?.ClipCount==2)
            { _session.SaveProjectAs(Path.Combine(_directory,"benchmark-template.cutcap")); _stage="save"; }
            else if(_stage=="save"&&view.Project?.Document?.State=="saved"&&view.Pending==0)
            {
                _largeFixture=Task.Run(()=>BuildLarge(Path.Combine(_directory,"benchmark-template.cutcap"),Path.Combine(_directory,"benchmark-5000.cutcap")));
                Plan(view);
            }
            else if(_stage=="seek")
            {
                if(view.TimelineGeneration!=_startTimelineGeneration) throw new InvalidOperationException("A seek rebuilt the clip document");
                if(_rendered) Next();
                else if(view.TimelinePreview is {Busy:false,State:"blank"} t&&t.PositionMs==_target)
                { _samples.Add(new {project=_project,targetMs=_target,blankLatencyMs=Stopwatch.GetElapsedTime(_requested).TotalMilliseconds,t.EvaluationUs,viewBytes=_session.LastViewBytes}); Next(); }
                else if(view.TimelinePreview is {State:"failed" or "no_frame"} failure) throw new InvalidOperationException(failure.Detail);
            }
            else if(_stage=="open-large"&&_largeFixture?.IsCompleted==true)
            { _session.OpenProject(_largeFixture.GetAwaiter().GetResult()); _stage="large-ready"; }
            else if(_stage=="large-ready"&&view.Project?.ClipCount==5000&&view.Media.All(m=>m.Availability=="ready")&&view.Pending==0&&view.TimelinePreview?.State=="ready")
            { _timelineBytes=_session.LastTimelineBytes; Plan(view,true); }
            else if(_stage=="rapid")
            {
                _project="rapid-16"; _target=1250; _oldGeneration=view.Frame?.Generation??0;
                _requested=Stopwatch.GetTimestamp(); _rendered=false;
                for(var i=0;i<15;i++) _session.SeekTimelineMilliseconds(i*100);
                _session.SeekTimelineMilliseconds(_target); _stage="rapid-wait"; _stageWatch.Restart();
            }
            else if(_stage=="rapid-wait"&&view.TimelinePreview is {State:"ready",PositionMs:1250} rapid)
            {
                _samples.Add(new {project="rapid-16",requests=16,readyLatencyMs=Stopwatch.GetElapsedTime(_requested).TotalMilliseconds,rapid.EvaluationUs,rapid.DecodeUs,rapid.ProcessId,rapid.Actual});
                if(Environment.GetEnvironmentVariable("CUTCAP_PROBE_MIN_LAYOUT")=="1")
                {
                    // Hold the genuine decoded still for native visual/keyboard
                    // inspection. Closing this isolated editor writes the report.
                    _stage="minimum-inspection"; _timer.Stop();
                    Console.WriteLine("Minimum layout ready for native inspection; close the editor to finish.");
                    return;
                }
                _session.SeekTimelineMilliseconds(1750); _stage="close-pending"; _stageWatch.Restart();
            }
            else if(_stage=="close-pending"&&view.TimelinePreview is {State:"loading"})
            {
                var pids=new List<int>();
                foreach(var child in Process.GetProcessesByName("ffmpeg")) { using(child) pids.Add(child.Id); }
                if(pids.Count>0)
                {
                    _samples.Add(new {project="close-during-decode",state="loading",childPids=pids});
                    Finish(desktop); return;
                }
            }
            if(_stageWatch.Elapsed>TimeSpan.FromSeconds(30)) throw new TimeoutException($"Timeline diagnostic stalled in {_stage}: {view.TimelinePreview}");
        }
        catch(Exception e)
        {
            _error=e.ToString(); Console.Error.WriteLine(e);
            if(Application.Current?.ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop) Finish(desktop);
        }
    }
    private void Finish(IClassicDesktopStyleApplicationLifetime desktop)
    {
        _timer.Stop(); if(_frame is not null) _frame.FrameRendered-=Rendered;
        if(desktop.MainWindow?.DataContext is MainWindowViewModel model) model.ConfirmUnsaved=()=>Task.FromResult(UnsavedChoice.Discard);
        desktop.Shutdown();
    }
}
