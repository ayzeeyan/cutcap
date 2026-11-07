# Phase 4B native A/V measurements (CutCap must own the foreground). Dot-sourced
# by av-bench-ui.ps1 through filmstrip-ui.ps1 -Perf -Stage. For each generated
# source (white flash on the first frame of every second, 10 ms 2 kHz click at
# every second) it places the file, waits for its waveform, then plays three
# short windows from non-whole positions, and finally plays the 180 s source
# for -LongSeconds to measure drift. Every number comes from:
#   * WASAPI loopback (cutcap-audio-loopback): click times dated by capture
#     packet performance-counter timestamps (the endpoint mix, before the DAC);
#   * GDI screen capture of the picture: flash times (sample interval reported);
#   * the app's own CUTCAP_TRACE_AUDIO / CUTCAP_TRACE_THUMBNAILS lines.
# Results: $Shots\av-bench.json (copied to docs/measurements by the operator).
. "$PSScriptRoot\av-common.ps1"
if (-not $LongSeconds) { $LongSeconds = 60 }

function Trace-Rows([string]$file, [double]$from, [double]$to) {
  @(Get-Content $file | Where-Object { $_ -like 'audio at=*' } | ForEach-Object {
      $row = @{}
      foreach ($m in [regex]::Matches($_, '(\w+)=(\S*)')) { $row[$m.Groups[1].Value] = $m.Groups[2].Value }
      [pscustomobject]$row
    } | Where-Object { [double]$_.unix_ms -ge $from -and [double]$_.unix_ms -le $to })
}
function Render-Rows([string]$file) {
  @(Get-Content $file | Where-Object { $_ -match 'render .*wave_cols=(\d+).*draw_us=(\d+)' } | ForEach-Object {
      if ($_ -match 'wave_cols=(\d+).*draw_us=(\d+)') { , @([int]$Matches[1], [double]$Matches[2]) } })
}
function Summary($values) {
  $v = @($values | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ } | Sort-Object)
  if ($v.Count -eq 0) { return $null }
  [ordered]@{
    n = $v.Count; min = $v[0]; median = $v[[int][Math]::Floor(($v.Count - 1) / 2)]
    p95 = $v[[int][Math]::Min($v.Count - 1, [Math]::Ceiling($v.Count * 0.95) - 1)]; max = $v[-1]
    mean = [Math]::Round(($v | Measure-Object -Average).Average, 2)
  }
}
# Least-squares slope (ms per minute) of y over wall time x (ms).
function Slope($pairs) {
  $p = @($pairs); if ($p.Count -lt 3) { return $null }
  $mx = ($p | ForEach-Object { $_[0] } | Measure-Object -Average).Average
  $my = ($p | ForEach-Object { $_[1] } | Measure-Object -Average).Average
  $num = 0.0; $den = 0.0
  foreach ($q in $p) { $num += ($q[0] - $mx) * ($q[1] - $my); $den += ($q[0] - $mx) * ($q[0] - $mx) }
  if ($den -eq 0) { return $null }
  [Math]::Round($num / $den * 60000, 3)
}
function Cpu-Seconds([int[]]$ids) {
  $sum = 0.0
  foreach ($p in @(Get-Process -Id $ids -ErrorAction SilentlyContinue)) { try { $sum += $p.TotalProcessorTime.TotalSeconds } catch { } }
  $sum
}

# One timed playback window from $fromMs for $seconds.
function Measure-Window([IntPtr]$h, $app, [string]$label, [double]$fromMs, [double]$seconds, $rect, [string]$trace) {
  Click-Point (Ruler-Point $h $fromMs); Start-Sleep -Milliseconds 1100
  $p0 = Secs (Playhead $h)
  $rec = Start-Recorder $label ($seconds + 2.5)
  Start-Sleep -Milliseconds 500
  $app.P.Refresh(); $cpu0 = $app.P.TotalProcessorTime.TotalSeconds
  $play = Timed-Click $h 'Play timeline'
  $screen = Sample-Flashes $h $seconds $rect
  $ff = @(Get-Process ffmpeg -ErrorAction SilentlyContinue | ForEach-Object Id)
  $ffCpu = Cpu-Seconds $ff
  $app.P.Refresh(); $cpu1 = $app.P.TotalProcessorTime.TotalSeconds
  $pause = Timed-Click $h 'Pause timeline'
  $wall = ($pause - $play) / 1000
  $heard = Read-Recorder $rec $label
  $clicks = @($heard.clicks_unix_ms | Where-Object { $_ -gt $play -and $_ -lt $pause })
  Start-Sleep -Milliseconds 400
  $rows = Trace-Rows $trace $play $pause
  $playing = @($rows | Where-Object { $_.state -eq 'Playing' -and $_.out_state -eq 'playing' })
  $first = $playing | Select-Object -First 1; $last = $playing | Select-Object -Last 1
  $clockOff = Clock-Offsets $trace $clicks $play $pause
  $avOff = AV-Offsets $clicks $screen.Flashes
  $intervals = @(for ($i = 1; $i -lt $clicks.Count; $i++) { [Math]::Round($clicks[$i] - $clicks[$i - 1], 2) })
  [ordered]@{
    label = $label; from_s = $p0; wall_s = [Math]::Round($wall, 3)
    loopback = [ordered]@{ device = $heard.device; rate = $heard.rate; rms = $heard.rms; clicks = $clicks.Count; clock_map_bracket_ms = $heard.clock_map_bracket_ms }
    start_latency_ms = Start-Latency $p0 $play $clicks
    click_minus_engine_clock_ms = Summary $clockOff
    click_minus_screen_flash_ms = Summary $avOff
    screen = [ordered]@{ flashes = $screen.Flashes.Count; sample_interval_ms = $screen.IntervalMs }
    click_interval_ms = Summary $intervals
    engine = [ordered]@{
      copied_frames = if ($first -and $last) { [int]$last.copied - [int]$first.copied } else { $null }
      copied_per_s = if ($first -and $last -and [double]$last.unix_ms -gt [double]$first.unix_ms) { [Math]::Round(([int]$last.copied - [int]$first.copied) / (([double]$last.unix_ms - [double]$first.unix_ms) / 1000), 2) } else { $null }
      video_late = if ($last) { [int]$last.video_late } else { $null }
      internal_av_ms = Summary ($playing | ForEach-Object { if ($_.av_ms) { $_.av_ms } })
      underruns = if ($last) { [int]$last.underruns } else { $null }
      padded_ms = if ($last) { [double]$last.padded_ms } else { $null }
      buffered_ms = Summary ($playing | ForEach-Object { $_.buffered_ms })
      compensation_ms = if ($last) { $last.comp_ms } else { $null }
      working_set_mib = Summary ($rows | ForEach-Object { $_.ws_mib })
    }
    cpu = [ordered]@{ app_cores = [Math]::Round(($cpu1 - $cpu0) / $wall, 3); ffmpeg_cpu_s_alive_at_pause = [Math]::Round($ffCpu, 2); ffmpeg_processes_at_pause = $ff.Count }
  }
}

# Closes without saving (placement dirtied the project); reports shutdown time.
function Close-Discard($app, [string]$label) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  [void]$app.P.CloseMainWindow()
  $prompt = $null
  for ($i = 0; $i -lt 20 -and -not $app.P.HasExited; $i++) {
    if (@(All-Elements $app.H | Where-Object { $_.Current.Name -eq 'Continue without saving' -and (Visible $_) }).Count) { $prompt = $w.ElapsedMilliseconds; Click-Named $app.H 'Continue without saving'; break }
    Start-Sleep -Milliseconds 100
  }
  $after = [Diagnostics.Stopwatch]::StartNew()
  $exited = $app.P.WaitForExit(10000)
  Start-Sleep -Milliseconds 400
  $left = @(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count
  Note "$label | exited=$exited, exit $($after.ElapsedMilliseconds) ms after the discard prompt (prompt at $prompt ms) leftover=$left"
  [ordered]@{ exited = $exited; exit_after_prompt_ms = $after.ElapsedMilliseconds; leftover_processes = $left }
}

function Place-And-Wait([IntPtr]$h, $app, [string]$file, [string]$name, [int]$copies = 1) {
  Import $h $app.P.Id $file
  $placed = UnixMs
  for ($i = 0; $i -lt $copies; $i++) {
    Select-Source $h ($name.Substring(0, 6)) $name
    $added = UnixMs
    Click-Named $h 'Add to timeline'; Start-Sleep -Milliseconds 300
  }
  $null = Wait-Lanes $h "$(2 * $copies) clips" 15000
  Click-Named $h 'Media collections'
  # Measured from the last Add to timeline press (the press itself is looked up first).
  $w = [Diagnostics.Stopwatch]::StartNew(); $wave = $null
  while ($w.ElapsedMilliseconds -lt 20000) {
    $px = (Wave-Pixels $h 1000 6000).Wave
    if ($px -gt 80) { $wave = [Math]::Round((UnixMs) - $added); break }
    Start-Sleep -Milliseconds 100
  }
  Click-Point (Ruler-Point $h 1500); Start-Sleep -Milliseconds 900  # Timeline preview mode
  [ordered]@{ waveform_visible_after_add_ms = $wave; import_unix_ms = $placed }
}

$bench = [ordered]@{
  generated_at = (Get-Date).ToString('o')
  machine = [ordered]@{
    os = [Environment]::OSVersion.VersionString; cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
    logical_cores = [Environment]::ProcessorCount; scale = $null
  }
  build = $(if ($Exe -like '*Release*') { 'Release desktop app over the release engine DLL' } else { 'Debug desktop app over the Debug engine DLL' })
  compensation_setting_ms = if ($env:CUTCAP_AUDIO_LATENCY_MS) { [int]$env:CUTCAP_AUDIO_LATENCY_MS } else { 0 }
  sources = @(); long = $null; many_clips = $null
}

$env:CUTCAP_TRACE_AUDIO = '1'; $env:CUTCAP_TRACE_THUMBNAILS = '1'
$script:launch = 1
foreach ($name in 'av sync 720p.mp4', 'av sync 1080p.mp4', 'av sync 4k.mp4', 'av vfr 720p.mkv') {
  $file = Join-Path $work $name
  Copy-Item (Join-Path $repo ".artifacts\av\sources\$name") $file
  $app = Start-App; $h = $app.H; $trace = "$Shots\app-out-$($script:launch - 1).txt"
  $bench.machine.scale = Scale $h
  $placed = Place-And-Wait $h $app $file $name
  $rect = Frame-Rect $h
  $tag = ($name -replace '[^a-z0-9]+', '-').Trim('-')
  $windows = @(
    foreach ($from in 1500, 3400, 5300) { Measure-Window $h $app "$tag-$from" $from 4.0 $rect $trace }
  )
  $render = @(Render-Rows $trace | Where-Object { $_[0] -gt 0 })
  $last = Trace-Rows $trace 0 ([double]::MaxValue) | Select-Object -Last 1
  $bench.sources += [ordered]@{
    file = $name; waveform = $placed; windows = $windows
    lanes_draw_us_with_waveform = Summary ($render | ForEach-Object { $_[1] })
    waveform_columns_drawn = Summary ($render | ForEach-Object { $_[0] })
    waveform_service = [ordered]@{ analysed = $last.wave_analysed; batches = $last.wave_batches; cancelled = $last.wave_cancelled; last_batch_ms = $last.wave_last_ms; cache_hits = $last.wave_cache_hits; cache_misses = $last.wave_cache_misses; cache_kib = $last.wave_cache_kib }
  }
  Note "$name waveform visible $($placed.waveform_visible_after_add_ms) ms after Add; start latency $(($windows | ForEach-Object { $_.start_latency_ms }) -join '/') ms; click-clock median $(($windows | ForEach-Object { $_.click_minus_engine_clock_ms.median }) -join '/') ms; click-flash median $(($windows | ForEach-Object { if ($_.click_minus_screen_flash_ms) { $_.click_minus_screen_flash_ms.median } }) -join '/') ms; copied/s $(($windows | ForEach-Object { $_.engine.copied_per_s }) -join '/'); late $(($windows | ForEach-Object { $_.engine.video_late }) -join '/'); underruns $(($windows | ForEach-Object { $_.engine.underruns }) -join '/'); cores $(($windows | ForEach-Object { $_.cpu.app_cores }) -join '/')"
  $bench.sources[-1].close = Close-Discard $app "close $name"
}

# Drift: one long playback of the 180 s source.
$name = 'av long 1080p.mp4'; $file = Join-Path $work $name
Copy-Item (Join-Path $repo ".artifacts\av\sources\$name") $file
$app = Start-App; $h = $app.H; $trace = "$Shots\app-out-$($script:launch - 1).txt"
$placed = Place-And-Wait $h $app $file $name
$rect = Frame-Rect $h
$long = Measure-Window $h $app 'long' 2500 $LongSeconds $rect $trace
$heard = Get-Content "$Shots\long.json" -Raw | ConvertFrom-Json
$clicks = @($heard.clicks_unix_ms)
$clockOff = @(foreach ($c in $clicks) { $o = Clock-Offsets $trace @($c) 0 ([double]::MaxValue); if ($o) { , @($c, $o[0]) } })
$flashFile = "$Shots\flash-samples-$($script:flashLog).csv"
$fl = [System.Collections.Generic.List[double]]::new(); $prev = $true
foreach ($line in Get-Content $flashFile) { $f = $line.Split(','); $wht = $f[4] -eq '1'; if ($wht -and -not $prev) { $fl.Add([double]$f[0]) }; $prev = $wht }
$avPairs = @(foreach ($c in $clicks) { $n = $fl | Sort-Object { [Math]::Abs($_ - $c) } | Select-Object -First 1; if ($null -ne $n -and [Math]::Abs($c - $n) -lt 400) { , @($c, ($c - $n)) } })
$t0 = if ($clicks.Count) { $clicks[0] } else { 0 }
$wallSpan = if ($clicks.Count -gt 1) { $clicks[-1] - $clicks[0] } else { 0 }
$long.drift = [ordered]@{
  clicks = $clicks.Count
  click_span_wall_ms = [Math]::Round($wallSpan, 2)
  click_span_media_ms = ($clicks.Count - 1) * 1000
  device_vs_system_clock_ppm = if ($clicks.Count -gt 1) { [Math]::Round(($wallSpan - ($clicks.Count - 1) * 1000) / (($clicks.Count - 1) * 1000) * 1e6, 1) } else { $null }
  click_minus_engine_clock_slope_ms_per_min = Slope ($clockOff | ForEach-Object { , @(($_[0] - $t0), $_[1]) })
  click_minus_flash_slope_ms_per_min = Slope ($avPairs | ForEach-Object { , @(($_[0] - $t0), $_[1]) })
  click_minus_engine_clock_first10 = Summary ($clockOff | Select-Object -First 10 | ForEach-Object { $_[1] })
  click_minus_engine_clock_last10 = Summary ($clockOff | Select-Object -Last 10 | ForEach-Object { $_[1] })
  click_minus_flash_first10 = Summary ($avPairs | Select-Object -First 10 | ForEach-Object { $_[1] })
  click_minus_flash_last10 = Summary ($avPairs | Select-Object -Last 10 | ForEach-Object { $_[1] })
}
$long.waveform = $placed
$bench.long = $long
Note "long $LongSeconds s: clicks=$($clicks.Count) device-vs-system $($long.drift.device_vs_system_clock_ppm) ppm; click-clock slope $($long.drift.click_minus_engine_clock_slope_ms_per_min) ms/min; click-flash slope $($long.drift.click_minus_flash_slope_ms_per_min) ms/min; underruns $($long.engine.underruns); late $($long.engine.video_late)"
$bench.long.close = Close-Discard $app 'close long'

# Many clips: 12 linked pairs of the 20 s 720p source back to back (24 clips),
# played across two cuts.
$name = 'av sync 720p.mp4'; $file = Join-Path $work $name
$app = Start-App; $h = $app.H; $trace = "$Shots\app-out-$($script:launch - 1).txt"
$placed = Place-And-Wait $h $app $file $name 12
$rect = Frame-Rect $h
$many = Measure-Window $h $app 'many' 18500 6.0 $rect $trace
$render = @(Render-Rows $trace | Where-Object { $_[0] -gt 0 })
$many.clips = Lanes $h
$many.waveform = $placed
$many.lanes_draw_us_with_waveform = Summary ($render | ForEach-Object { $_[1] })
$bench.many_clips = $many
Note "many clips ($($many.clips)): clicks $($many.loopback.clicks), intervals $($many.click_interval_ms.min)..$($many.click_interval_ms.max) ms, underruns $($many.engine.underruns), late $($many.engine.video_late), draw_us median $($many.lanes_draw_us_with_waveform.median)"
$bench.many_clips.close = Close-Discard $app 'close many'
$env:CUTCAP_TRACE_AUDIO = $null; $env:CUTCAP_TRACE_THUMBNAILS = $null
$bench.leftover_processes = @(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count
$bench | ConvertTo-Json -Depth 8 | Set-Content "$Shots\av-bench.json" -Encoding utf8
$log | Set-Content "$Shots\av-bench.txt" -Encoding utf8
