# Phase 4B native audio/video scenario. Dot-sourced by av-ui.ps1 through
# filmstrip-ui.ps1 -Perf -Stage (reuses its helpers and the foreground guard).
# Source: a generated 20 s 1280x720 H.264 + 48 kHz AAC file with a full-frame
# white flash on the first frame of every second and a 10 ms 2 kHz click at
# every whole second. Audible output is verified by WASAPI loopback
# (cutcap-audio-loopback), the picture by sampling the screen.
$avName = 'av sync 720p.mp4'
$av = Join-Path $work $avName
Copy-Item (Join-Path $repo ".artifacts\av\sources\$avName") $av
$project = Join-Path $work 'av session.cutcap'
. "$PSScriptRoot\av-common.ps1"

$env:CUTCAP_TRACE_AUDIO = '1'; $env:CUTCAP_TRACE_THUMBNAILS = '1'
$script:launch = 1
$app = Start-App; $h = $app.H
$env:CUTCAP_TRACE_AUDIO = $null; $env:CUTCAP_TRACE_THUMBNAILS = $null
Note "scale=$(Scale $h) audio latency compensation setting CUTCAP_AUDIO_LATENCY_MS=$(if ($env:CUTCAP_AUDIO_LATENCY_MS) { $env:CUTCAP_AUDIO_LATENCY_MS } else { "unset (0)" })"

# 1-2. Import the A/V source; its audio stream details are shown; drag it onto Video 1 at 0.
Import $h $app.P.Id $av
$from = Tile $h 'av sync' $avName
Start-Sleep -Milliseconds 600
$audioRow = All-Elements $h | Where-Object { $_.Current.Name -like '* Hz *' } | Select-Object -First 1
Note "inspector audio row: '$($audioRow.Current.Name)'"
Drag-Points $from (Lane-Point $h 300 0) 30
Clear-Search $h
Note "placed by drag: $(Wait-Lanes $h '2 clips')"

# 3-4. The linked audio sits on Audio 1 and shows a measured waveform.
Start-Sleep -Milliseconds 6000
Click-Named $h 'Media collections'
$wave = Wave-Pixels $h 1000 19000
Note "audio lane pixels: waveform=$($wave.Wave) navy=$($wave.Navy) (linked audio clip with peaks: both > 0)"
Save-Client $h "$Shots\01-linked-waveform.jpg"

# 5-7. Play through the Windows output: loopback must hear the clicks; pause and resume.
Click-Point (Ruler-Point $h 1000); Start-Sleep -Milliseconds 1200
$p0 = Secs (Playhead $h)
$rec = Start-Recorder 'play' 13
Start-Sleep -Milliseconds 600
$frameRect = Frame-Rect $h
Note "picture probe area: $($frameRect.X),$($frameRect.Y) $($frameRect.Width)x$($frameRect.Height)"
$playAt = Timed-Click $h 'Play timeline'; Note 'MARK play'
$screen = Sample-Flashes $h 4.5 $frameRect
Save-Client $h "$Shots\02-playing.jpg"
$pauseAt = Timed-Click $h 'Pause timeline'; Note 'MARK pause'
Start-Sleep -Milliseconds 1500
$paused = (Playhead $h)
Start-Sleep -Milliseconds 700
Note "playhead while paused: '$paused' then '$((Playhead $h))'"
$resumeAt = Timed-Click $h 'Play timeline'; Note 'MARK resume'
Start-Sleep -Milliseconds 1500
Click-Named $h 'Pause timeline'; Note 'MARK pause2'
$heard = Read-Recorder $rec 'play'
$clicks = @($heard.clicks_unix_ms)
$duringPause = @($clicks | Where-Object { $_ -gt $pauseAt + 150 -and $_ -lt $resumeAt })
Note "loopback device='$($heard.device)' rate=$($heard.rate) rms=$($heard.rms) clicks=$($clicks.Count) from playhead $p0 s: first click $([Math]::Round($clicks[0] - $playAt)) ms after Play, start latency $(Start-Latency $p0 $playAt $clicks) ms; clicks during pause=$($duringPause.Count)"
$afterResume = @($clicks | Where-Object { $_ -gt $resumeAt })
Note "resume latency $(Start-Latency (Secs $paused) $resumeAt $afterResume) ms"
Note "audible click minus engine clock (ms): $(Stats (Clock-Offsets "$Shots\app-out-1.txt" $clicks $playAt ($resumeAt + 3000)))"
$offsets = AV-Offsets ($clicks | Where-Object { $_ -lt $pauseAt }) $screen.Flashes
Note "screen samples=$($screen.Samples) every $($screen.IntervalMs) ms, flashes=$($screen.Flashes.Count); A/V offset click-flash (ms): $(Stats $offsets)"

# 8. Seek to known positions: playing from 6 s, the first click is 6 s on the source clock.
Click-Point (Ruler-Point $h 6000); Start-Sleep -Milliseconds 1200
$p0 = Secs (Playhead $h)
$rec = Start-Recorder 'seek' 4
Start-Sleep -Milliseconds 500
$seekPlay = Timed-Click $h 'Play timeline'
Start-Sleep -Milliseconds 2500
Click-Named $h 'Pause timeline'
$seek = Read-Recorder $rec 'seek'
Note "after seek to $p0 s: clicks=$(@($seek.clicks_unix_ms).Count) start latency $(Start-Latency $p0 $seekPlay $seek.clicks_unix_ms) ms; playhead after pause '$((Playhead $h))'"

# 9. Split the linked pair at 10 s (Ctrl+B splits both).
Click-Point (Ruler-Point $h 10000); Start-Sleep -Milliseconds 900
Click-Point (Lane-Point $h 4000 0); Click-Named $h 'Media collections'
Send-Key $h 'CTRL+B'; Note "split: $(Wait-Lanes $h '4 clips')"
# 10. Trim the right pair's start by 1 s from the audio member (11 s).
$p0 = Lane-Point $h 10000 1; $p0.X += [int](3 * (Scale $h))
Drag-Points $p0 (Lane-Point $h 11000 1) 20
Note "trimmed from audio: $(Wait-Lanes $h '00:11.000 to 00:20.000')"
# 11. Move the right pair to 13 s by dragging its video member.
Drag-Points (Lane-Point $h 15000 0) (Lane-Point $h 17000 0)
Note "moved: $(Wait-Lanes $h '00:13.000 to 00:22.000')"
Start-Sleep -Milliseconds 2500
Save-Client $h "$Shots\03-split-trim-move.jpg"
# 12. Undo / redo the move: both members follow.
Send-Key $h 'CTRL+Z'; Note "undo move: $(Wait-Lanes $h '00:11.000 to 00:20.000')"
Send-Key $h 'CTRL+Y'; Note "redo move: $(Wait-Lanes $h '00:13.000 to 00:22.000')"

# 13-14. Play across the edited cut: clicks before 10 s, silence in the 10-13 s gap,
# then source seconds 11.. at timeline 13..; flashes and clicks stay together.
Click-Point (Ruler-Point $h 8000); Start-Sleep -Milliseconds 1200
$p0 = Secs (Playhead $h)
$rec = Start-Recorder 'cut' 9
Start-Sleep -Milliseconds 500
$cutPlay = Timed-Click $h 'Play timeline'
$screen = Sample-Flashes $h 7.5 $frameRect
Click-Named $h 'Pause timeline'
$cut = Read-Recorder $rec 'cut'
$rel = @($cut.clicks_unix_ms | ForEach-Object { [Math]::Round($_ - $cutPlay - (1000 * ([Math]::Ceiling($p0 - 0.002) - $p0))) })
Note "across the cut from $p0 s: clicks at +$($rel -join ', +') ms after Play minus the media time to the first whole second (timeline 8 s, 9 s, then 13 s, 14 s ...: expect +L, then +4000+L, +5000+L; none for the 10-13 s gap)"
Note "across the cut A/V offset (ms): $(Stats (AV-Offsets $cut.clicks_unix_ms $screen.Flashes)), flashes=$($screen.Flashes.Count)"

# 15-16. Save, close, reopen: pairs, links and timing come back; waveforms regenerate.
Send-Key $h 'CTRL+SHIFT+S'; Start-Sleep -Milliseconds 900
Complete-Dialog $app.P.Id 'Save CutCap project' $project; Start-Sleep -Milliseconds 2000
$saved = Clips-In-File
Note "saved clips: $($saved -join ' ; ')"
Close-App $app 'close after save'
$env:CUTCAP_TRACE_AUDIO = '1'
$app = Start-App; $h = $app.H
$env:CUTCAP_TRACE_AUDIO = $null
Open-Project $h $app.P.Id
Note "reopened: $(Wait-Lanes $h '4 clips' 15000)"
Start-Sleep -Milliseconds 7000
$wave = Wave-Pixels $h 1000 9000
Note "reopened title='$((Get-Process -Id $script:CutCapPid).MainWindowTitle)' waveform pixels=$($wave.Wave) file unchanged=$(((Clips-In-File) -join ';') -eq ($saved -join ';'))"
Save-Client $h "$Shots\04-reopened.jpg"
Click-Point (Ruler-Point $h 1000); Start-Sleep -Milliseconds 1200
$rec = Start-Recorder 'reopened' 4
Start-Sleep -Milliseconds 400
$reopenPlay = UnixMs; Click-Named $h 'Play timeline'; Start-Sleep -Milliseconds 2500; Click-Named $h 'Pause timeline'
$re = Read-Recorder $rec 'reopened'
Note "reopened playback: rms=$($re.rms) clicks=$(@($re.clicks_unix_ms).Count)"
Close-App $app 'close after reopen'

# 17-18. Missing media: silent, honest state; relink restores audio and waveforms.
Rename-Item -LiteralPath $av 'av moved.mp4'
$app = Start-App; $h = $app.H
Open-Project $h $app.P.Id
Note "reopened with missing source: $(Wait-Lanes $h '4 clips' 15000)"
Start-Sleep -Milliseconds 3000
Save-Client $h "$Shots\05-missing.jpg"
Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Type-Text $h 'av'; Start-Sleep -Milliseconds 500
Click-Named $h $avName; Start-Sleep -Milliseconds 500
Click-Named $h 'Project menu'; Start-Sleep -Milliseconds 500
Click-Named $h 'Relink selected source…'; Start-Sleep -Milliseconds 900
Complete-Dialog $app.P.Id 'Relink original source video' (Join-Path $work 'av moved.mp4')
Start-Sleep -Milliseconds 6000
Clear-Search $h
Start-Sleep -Milliseconds 4000
$wave = Wave-Pixels $h 1000 9000
Note "relinked: waveform pixels=$($wave.Wave)"
Save-Client $h "$Shots\06-relinked.jpg"
Send-Key $h 'CTRL+S'; Start-Sleep -Milliseconds 1500
Close-App $app 'close after relink'

# 19. An old (schema 1) video-only project saved by the Phase 3D build still opens
# and plays; opening alone does not rewrite the file.
$old = Join-Path $work 'phase3d two-source.cutcap'
Copy-Item (Join-Path $repo '.artifacts\phase3d\native-two-source.cutcap') $old
Copy-Item (Join-Path $repo '.artifacts\phase3d\native-media') (Join-Path $work 'native-media') -Recurse
$oldHash = (Get-FileHash -LiteralPath $old).Hash
$app = Start-App; $h = $app.H
$project = $old
Open-Project $h $app.P.Id
Note "old schema 1 project: $(Wait-Lanes $h '2 clips' 15000) title='$((Get-Process -Id $script:CutCapPid).MainWindowTitle)' lanes='$(Lanes $h)'"
Click-Point (Ruler-Point $h 500); Start-Sleep -Milliseconds 1000
Click-Named $h 'Play timeline'; Start-Sleep -Milliseconds 1500
Note "old project playing: playhead '$((Playhead $h))'"
Save-Client $h "$Shots\07-old-project.jpg"
Click-Named $h 'Pause timeline'
Close-App $app 'close old project'
Note "old project file unchanged by opening: $((Get-FileHash -LiteralPath $old).Hash -eq $oldHash)"

# 20-21. Close during playback and waveform analysis: no orphans, device released.
$project = Join-Path $work 'av session.cutcap'
$app = Start-App; $h = $app.H
Open-Project $h $app.P.Id
$null = Wait-Lanes $h '4 clips' 15000
Click-Point (Ruler-Point $h 2000); Start-Sleep -Milliseconds 800
Click-Named $h 'Zoom in'; Click-Named $h 'Play timeline'; Start-Sleep -Milliseconds 1200
Close-App $app 'close during playback and waveform analysis'
$rec = Start-Recorder 'after-close' 2
$after = Read-Recorder $rec 'after-close'
Note "after close: CutCap/FFmpeg/FFprobe processes=$(@(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count) output rms=$($after.rms) (silent: the device stream was released)"
$log | Set-Content "$Shots\av-scenario.txt" -Encoding utf8
