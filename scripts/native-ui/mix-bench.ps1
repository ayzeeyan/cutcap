# Phase 4C native mixer measurements (CutCap must own the foreground). Dot-sourced
# by mix-bench-ui.ps1 through filmstrip-ui.ps1 -Perf -Stage. For each track count
# the timeline holds the 180 s 1080p sync source as a linked pair (flash and 2 kHz
# click every second) plus N-1 independent AAC tone tracks (one FFmpeg decoder
# each), summed well below full scale. Everything comes from WASAPI loopback,
# screen capture of the picture, and the app's own trace lines.
. "$PSScriptRoot\av-common.ps1"
if (-not $LongSeconds) { $LongSeconds = 60 }
$syncName = 'av long 1080p.mp4'; $sync = Join-Path $work $syncName
Copy-Item (Join-Path $repo ".artifacts\av\sources\$syncName") $sync
$toneName = 'mix bench tone.m4a'; $tone = Join-Path $work $toneName
Copy-Item (Join-Path $repo ".artifacts\av\sources-mix\$toneName") $tone

$bench = [ordered]@{
  generated_at = (Get-Date).ToString('o')
  machine = [ordered]@{
    os = [Environment]::OSVersion.VersionString; cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
    logical_cores = [Environment]::ProcessorCount
  }
  build = $(if ($Exe -like '*Release*') { 'Release desktop app over the release engine DLL' } else { 'Debug desktop app over the Debug engine DLL' })
  compensation_setting_ms = if ($env:CUTCAP_AUDIO_LATENCY_MS) { [int]$env:CUTCAP_AUDIO_LATENCY_MS } else { 0 }
  runs = @(); one_tone_track = $null; limit = $null; long = $null
}

# Once a source has been previewed, the monitor and inspector titles carry its
# name too, so the tile is looked up inside the library list only.
function Select-Tile([IntPtr]$h, [string]$fragment, [string]$name) {
  Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Type-Text $h $fragment; Start-Sleep -Milliseconds 500
  $list = Find-Element $h 'Imported media'
  $tile = @($list.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
    Where-Object { ($_.Current.Name -eq $name -or $_.Current.Name.StartsWith("$name,")) -and (Visible $_) } | Select-Object -First 1)
  if (-not $tile.Count) { throw "no library tile named '$name'" }
  $r = $tile[0].Current.BoundingRectangle
  Click-Point @{ X = [int]($r.X + $r.Width / 2); Y = [int]($r.Y + $r.Height / 2) }; Start-Sleep -Milliseconds 700
  Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Send-Key $h 'BACK'; Start-Sleep -Milliseconds 500
}

# A timeline of the sync pair and N-1 tone tracks, placed through real UI.
function Build-Timeline([IntPtr]$h, $app, [int]$tracks) {
  Import $h $app.P.Id $sync
  $w = [Diagnostics.Stopwatch]::StartNew()
  Select-Source $h 'av long' $syncName
  Click-Named $h 'Add to timeline'; Start-Sleep -Milliseconds 300
  $null = Wait-Lanes $h '2 clips' 15000
  if ($tracks -gt 1) {
    Import $h $app.P.Id $tone
    for ($i = 2; $i -le $tracks; $i++) { Click-Named $h 'Add audio track'; Start-Sleep -Milliseconds 350 }
    Select-Source $h 'mix bench' $toneName
    Click-Point (Ruler-Point $h 0); Start-Sleep -Milliseconds 800
    Select-Source $h 'mix bench' $toneName
    for ($i = 2; $i -le $tracks; $i++) {
      Click-Named $h 'Place audio stream 0 on the timeline'; Start-Sleep -Milliseconds 350
      $null = Wait-Lanes $h "$($i + 1) clips" 15000
      Select-Tile $h 'mix bench' $toneName
    }
  }
  Click-Named $h 'Media collections'
  Click-Point (Ruler-Point $h 1500); Start-Sleep -Milliseconds 900
  $w.ElapsedMilliseconds
}

# Time from an action to the midpoint between the levels measured either side of
# it, held for 100 ms. Null when there was no step to see.
function Step-Response($d, [double]$at) {
  $before = Tone-Mean $d '700' ($at - 700) ($at - 100)
  $after = Tone-Mean $d '700' ($at + 700) ($at + 1200)
  $ms = (Tone-Cross $d '700' $at (($before + $after) / 2) ($after -gt $before) 1200 10) - $at
  if ([double]::IsNaN($ms)) { $null } else { [Math]::Round($ms, 1) }
}

# After a window: seek during playback, then the controls, on the loopback clock.
# Solo comes first: it leaves one clean tone, because the sync source's clicks stop
# and removing one tone of many is otherwise too small a step to see. The last
# track is the one still in view after adding them all. $tone is false only for the
# run whose single track is the click source.
function Measure-Controls([IntPtr]$h, $app, [string]$label, [int]$tracks, [bool]$tone = $true) {
  Click-Point (Ruler-Point $h 8200); Start-Sleep -Milliseconds 1000
  $rec = Start-ToneRecorder $label 15 '700'
  Start-Sleep -Milliseconds 400
  $null = Timed-Click $h 'Play timeline'
  Start-Sleep -Milliseconds 3000
  $ruler = Ruler-Point $h 20600
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos($ruler.X, $ruler.Y); Start-Sleep -Milliseconds 60
  $seek = UnixMs
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 30; [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 2800
  $solo = 0.0; $volume = 0.0; $mute = 0.0
  if ($tone) {
    $solo = Timed-Click $h "Solo Audio $tracks"; Start-Sleep -Milliseconds 1400
    $volume = Drag-Gain $h "Audio $tracks" 0 -12 1; Start-Sleep -Milliseconds 1400
    $mute = Timed-Click $h "Mute Audio $tracks"; Start-Sleep -Milliseconds 1400
  }
  $null = Timed-Click $h 'Pause timeline'
  $d = Read-Recorder $rec $label
  # The first click after the seek lands on a whole second of the source.
  $after = @($d.clicks_unix_ms | Where-Object { $_ -gt $seek + 80 })
  [ordered]@{
    seek_to_audio_ms = if ($after.Count) { [Math]::Round($after[0] - $seek - 400, 1) } else { $null }
    # A sole tone track has nothing else to silence, so solo changes nothing to measure.
    solo_response_ms = if ($tone -and $tracks -ge 3) { Step-Response $d $solo } else { $null }
    volume_response_ms = if ($tone) { Step-Response $d $volume } else { $null }
    mute_response_ms = if ($tone) { Step-Response $d $mute } else { $null }
  }
}

$env:CUTCAP_TRACE_AUDIO = '1'; $env:CUTCAP_TRACE_THUMBNAILS = '1'
$script:launch = 1

# One track holding only the tone, so a lone track's controls can be measured.
$app = Start-App; $h = $app.H
Import $h $app.P.Id $tone
Select-Source $h 'mix bench' $toneName
Click-Named $h 'Add to timeline'; Start-Sleep -Milliseconds 300
$null = Wait-Lanes $h '1 clip' 15000
Click-Named $h 'Media collections'
Click-Point (Ruler-Point $h 1500); Start-Sleep -Milliseconds 900
$bench.one_tone_track = Measure-Controls $h $app 'n1-tone-controls' 1
Note "n=1, tone only: volume $($bench.one_tone_track.volume_response_ms) ms, mute $($bench.one_tone_track.mute_response_ms) ms"
$null = Close-Discard $app 'close n=1 tone'

foreach ($tracks in 1, 2, 4, 8, 16) {
  $app = Start-App; $h = $app.H; $trace = "$Shots\app-out-$($script:launch - 1).txt"
  $built = Build-Timeline $h $app $tracks
  $rect = Frame-Rect $h
  $tag = "n$tracks"
  Save-Client $h "$Shots\$tag-timeline.jpg"
  $window = Measure-Window $h $app "$tag-play" 1500 6.0 $rect $trace
  $controls = Measure-Controls $h $app "$tag-controls" $tracks ($tracks -gt 1)
  $rows = Trace-Rows $trace 0 ([double]::MaxValue)
  $last = $rows | Where-Object { $_.blocks } | Select-Object -Last 1
  $render = @(Render-Rows $trace | Where-Object { $_[0] -gt 0 })
  $ffmpeg = @(Get-Process ffmpeg -ErrorAction SilentlyContinue)
  $close = Close-Discard $app "close n=$tracks"
  $run = [ordered]@{
    tracks = $tracks; build_ms = $built; window = $window; controls = $controls; close = $close
    trace_at_end = [ordered]@{
      decoders_peak = [int]$last.decoders_peak; mixed_peak = [int]$last.mixed_peak; clipped = [long]$last.clipped
      over_limit = [long]$last.over_limit; blocks = [long]$last.blocks; block_us_total = [long]$last.block_us_total
      block_us_mean = if ([long]$last.blocks -gt 0) { [Math]::Round([long]$last.block_us_total / [long]$last.blocks, 1) } else { $null }
      block_us_max = [long]$last.block_us_max
      period_us_last = [long]$last.period_us_last; period_us_max = [long]$last.period_us_max; periods = [long]$last.periods
    }
    mix_cpu_percent_of_realtime = if ([long]$last.blocks -gt 0) { [Math]::Round([long]$last.block_us_total / ([long]$last.blocks * 10000) * 100, 3) } else { $null }
    lanes_draw_us = Summary ($render | ForEach-Object { $_[1] })
    waveform_columns = Summary ($render | ForEach-Object { $_[0] })
  }
  $bench.runs += $run
  Note ("n={0}: underruns {1} late {2} | click-clock median {3} ms, click-flash median {4} ms | start {5} ms, seek {6} ms, solo {16} ms, volume {17} ms, mute {7} ms | app cores {8}, decoders peak {9}, mixed peak {10}, clipped {11} | mix {12}% of real time (block mean {13} us, max {14} us), device period max {15} us" -f $tracks, $window.engine.underruns, $window.engine.video_late, $window.click_minus_engine_clock_ms.median, $window.click_minus_screen_flash_ms.median, $window.start_latency_ms, $controls.seek_to_audio_ms, $controls.mute_response_ms, $window.cpu.app_cores, $run.trace_at_end.decoders_peak, $run.trace_at_end.mixed_peak, $run.trace_at_end.clipped, $run.mix_cpu_percent_of_realtime, $run.trace_at_end.block_us_mean, $run.trace_at_end.block_us_max, $run.trace_at_end.period_us_max, $controls.solo_response_ms, $controls.volume_response_ms)
}

# Over the limit: 17 overlapping audible tracks are refused, by name, at Play.
$app = Start-App; $h = $app.H
$null = Build-Timeline $h $app 17
$null = Timed-Click $h 'Play timeline'
Start-Sleep -Milliseconds 2500
$status = (All-Elements $h | Where-Object { $_.Current.Name -like '*audible audio tracks overlap*' } | Select-Object -First 1).Current.Name
Note "limit: at 17 tracks the app says: '$status'"
Save-Client $h "$Shots\limit-17.jpg"
$bench.limit = [ordered]@{ tracks = 17; message = $status; ffmpeg_processes_while_refused = @(Get-Process ffmpeg -ErrorAction SilentlyContinue).Count }
$bench.limit.close = Close-Discard $app 'close limit'

# Long session with four tracks.
$app = Start-App; $h = $app.H; $trace = "$Shots\app-out-$($script:launch - 1).txt"
$null = Build-Timeline $h $app 4
$rect = Frame-Rect $h
$long = Measure-Window $h $app 'long' 2500 $LongSeconds $rect $trace
$heard = Get-Content "$Shots\long.json" -Raw | ConvertFrom-Json
$clicks = @($heard.clicks_unix_ms)
$clockOff = @(foreach ($c in $clicks) { $o = Clock-Offsets $trace @($c) 0 ([double]::MaxValue); if ($o) { , @($c, $o[0]) } })
$t0 = if ($clicks.Count) { $clicks[0] } else { 0 }
$wallSpan = if ($clicks.Count -gt 1) { $clicks[-1] - $clicks[0] } else { 0 }
$long.drift = [ordered]@{
  clicks = $clicks.Count
  click_span_wall_ms = [Math]::Round($wallSpan, 2)
  device_vs_system_clock_ppm = if ($clicks.Count -gt 1) { [Math]::Round(($wallSpan - ($clicks.Count - 1) * 1000) / (($clicks.Count - 1) * 1000) * 1e6, 1) } else { $null }
  click_minus_engine_clock_slope_ms_per_min = Slope ($clockOff | ForEach-Object { , @(($_[0] - $t0), $_[1]) })
  click_minus_engine_clock_first10 = Summary ($clockOff | Select-Object -First 10 | ForEach-Object { $_[1] })
  click_minus_engine_clock_last10 = Summary ($clockOff | Select-Object -Last 10 | ForEach-Object { $_[1] })
}
$bench.long = $long
Note "long $LongSeconds s, 4 tracks: clicks=$($clicks.Count), device-vs-system $($long.drift.device_vs_system_clock_ppm) ppm, click-clock slope $($long.drift.click_minus_engine_clock_slope_ms_per_min) ms/min, underruns $($long.engine.underruns), late $($long.engine.video_late), peak working set $($long.engine.working_set_mib.max) MiB"
$bench.long.close = Close-Discard $app 'close long'
$env:CUTCAP_TRACE_AUDIO = $null; $env:CUTCAP_TRACE_THUMBNAILS = $null
$bench.leftover_processes = @(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count
$bench | ConvertTo-Json -Depth 8 | Set-Content "$Shots\mix-bench.json" -Encoding utf8
$log | Set-Content "$Shots\mix-bench.txt" -Encoding utf8
