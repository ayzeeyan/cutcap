# Shared helpers for the Phase 4B native stages (av-scenario.ps1, av-bench.ps1).
# Dot-sourced after filmstrip-ui.ps1's helpers.
$loopback = Join-Path $repo 'target\debug\cutcap-audio-loopback.exe'
if (-not (Test-Path $loopback)) { throw "Build the recorder first: cargo build -p cutcap-ffi --features audio-diagnostics --bin cutcap-audio-loopback" }
Add-Type -AssemblyName System.Drawing
# Filters the library so the tile is on screen; returns its centre.
function Tile([IntPtr]$h, [string]$fragment, [string]$name) {
  Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Type-Text $h $fragment; Start-Sleep -Milliseconds 500
  $r = (Find-Element $h $name).Current.BoundingRectangle
  @{ X = [int]($r.X + $r.Width / 2); Y = [int]($r.Y + $r.Height / 3) }
}
function Playhead([IntPtr]$h) {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'TimelinePlayhead')
  ([System.Windows.Automation.AutomationElement]::FromHandle($h)).FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c).Current.Name
}
function Clear-Search([IntPtr]$h) { Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Send-Key $h 'BACK'; Start-Sleep -Milliseconds 400 }

function UnixMs { ([DateTime]::UtcNow - [DateTime]::UnixEpoch).TotalMilliseconds }
function Start-Recorder([string]$name, [double]$seconds) {
  Start-Process $loopback -ArgumentList ('"' + "$Shots\$name.wav" + '" ' + $seconds) -RedirectStandardOutput "$Shots\$name.json" -PassThru -NoNewWindow
}
function Read-Recorder($proc, [string]$name) {
  $null = $proc.WaitForExit(60000)
  Get-Content "$Shots\$name.json" -Raw | ConvertFrom-Json
}
# The picture area: FrameView has no automation peer, so it is bracketed by the
# player status line above it and the position slider below it.
function Frame-Rect([IntPtr]$h) {
  $slider = (Find-Element $h 'Timeline position').Current.BoundingRectangle
  $status = (All-Elements $h | Where-Object { $b = $_.Current.BoundingRectangle; $_.Current.ClassName -eq 'TextBlock' -and [Math]::Abs($b.X - $slider.X) -lt 2 -and [Math]::Abs($b.Width - $slider.Width) -lt 2 -and $b.Y -lt $slider.Y } | Select-Object -First 1).Current.BoundingRectangle
  $top = $status.Y + $status.Height
  @{ X = $status.X; Y = $top; Width = $status.Width; Height = $slider.Y - $top }
}
# Clicks a named button and returns the Unix time of the mouse press (the lookup happens first).
function Timed-Click([IntPtr]$h, [string]$name) {
  $r = (Find-Element $h $name).Current.BoundingRectangle
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2)); Start-Sleep -Milliseconds 60
  $t = UnixMs
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 30; [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  $t
}
# Audible click vs the engine's own clock: the CUTCAP_TRACE_AUDIO trace samples
# (unix_ms, pos_ms) every 250 ms while playing; interpolate when the clock said T.
function Clock-Offsets([string]$trace, $clicks, [double]$fromMs, [double]$toMs) {
  $pts = @(Get-Content $trace | Where-Object { $_ -match 'unix_ms=([\d.]+) state=Playing clock=audio pos_ms=(\d+).*out_state=playing' } | ForEach-Object {
      if ($_ -match 'unix_ms=([\d.]+) state=Playing clock=audio pos_ms=(\d+)') { , @([double]$Matches[1], [double]$Matches[2]) } } |
      Where-Object { $_[0] -ge $fromMs -and $_[0] -le $toMs })
  @(foreach ($c in $clicks) {
      for ($i = 1; $i -lt $pts.Count; $i++) {
        $a = $pts[$i - 1]; $b = $pts[$i]
        if ($b[1] -le $a[1] -or $b[0] - $a[0] -gt 400) { continue }
        $T = [Math]::Round(($a[1] + ($c - $a[0]) * ($b[1] - $a[1]) / ($b[0] - $a[0])) / 1000) * 1000
        if ($T -ge $a[1] -and $T -le $b[1]) {
          $clockAt = $a[0] + ($T - $a[1]) * ($b[0] - $a[0]) / ($b[1] - $a[1])
          [Math]::Round($c - $clockAt, 1); break
        }
      }
    })
}
function Secs([string]$clock) { if ($clock -match '(\d+):(\d+\.\d+)') { [int]$Matches[1] * 60 + [double]$Matches[2] } else { [double]::NaN } }
# Wall time from Play to the first audible click, minus the media time until that click.
function Start-Latency([double]$p0, [double]$playMs, $clicks, [double]$clickAt = [double]::NaN) {
  if (-not $clicks -or @($clicks).Count -eq 0) { return 'no click' }
  if ([double]::IsNaN($clickAt)) { $clickAt = [Math]::Ceiling($p0 - 0.002) }
  [Math]::Round(@($clicks)[0] - $playMs - ($clickAt - $p0) * 1000, 1)
}
# Full-frame white flashes on screen. One GDI capture per sample (each capture
# costs about one display refresh, so probes in separate captures would span
# longer than a 33 ms flash): a thin strip across the middle of the picture,
# white at 20%, 50% and 80% of its width. A frame already white when sampling
# starts (a paused flash frame) is not counted. A flash is dated by the start
# of the capture that first saw it (resolution: the reported sample interval).
function Sample-Flashes([IntPtr]$h, [double]$seconds, $r) {
  $w = [int]($r.Width * 0.8); $x0 = [int]($r.X + $r.Width * 0.1); $y0 = [int]($r.Y + $r.Height / 2)
  $bmp = New-Object System.Drawing.Bitmap $w, 2
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $size = New-Object System.Drawing.Size $w, 2
  $probe = @([int]($w * 0.125), [int]($w * 0.5), [int]($w * 0.875))
  $flashes = [System.Collections.Generic.List[double]]::new()
  $samples = 0; $white = $true; $raw = [System.Collections.Generic.List[string]]::new()
  $end = (UnixMs) + $seconds * 1000
  while ((UnixMs) -lt $end) {
    $t = UnixMs
    $g.CopyFromScreen($x0, $y0, 0, 0, $size)
    $all = $true
    foreach ($x in $probe) { $c = $bmp.GetPixel($x, 0); if ($c.R -lt 230 -or $c.G -lt 230 -or $c.B -lt 230) { $all = $false } }
    $m = $bmp.GetPixel($probe[1], 0); $raw.Add("$([Math]::Round($t, 1)),$($m.R),$($m.G),$($m.B),$([int]$all)")
    if ($all -and -not $white) { $flashes.Add($t) }
    $white = $all; $samples++
  }
  $g.Dispose(); $bmp.Dispose()
  $script:flashLog++; $raw | Set-Content "$Shots\flash-samples-$($script:flashLog).csv"
  @{ Flashes = $flashes; Samples = $samples; Seconds = $seconds; IntervalMs = [Math]::Round($seconds * 1000 / [Math]::Max(1, $samples), 1) }
}
# Pairs each audible click with the nearest on-screen flash: offset = click - flash (ms).
function AV-Offsets($clicks, $flashes) {
  @($clicks | ForEach-Object {
      $c = [double]$_
      $nearest = $flashes | Sort-Object { [Math]::Abs($_ - $c) } | Select-Object -First 1
      if ($null -ne $nearest -and [Math]::Abs($c - $nearest) -lt 400) { [Math]::Round($c - $nearest, 1) }
    })
}
function Stats($values) {
  if (-not $values -or $values.Count -eq 0) { return 'n=0' }
  $s = $values | Sort-Object
  "n=$($s.Count) min=$($s[0]) median=$($s[[int]($s.Count / 2)]) max=$($s[-1]) mean=$([Math]::Round(($s | Measure-Object -Average).Average, 1))"
}
# Pixels of an audio clip: navy fill and light-blue peaks (waveform columns).
function Wave-Pixels([IntPtr]$h, [double]$fromMs, [double]$toMs, [int]$track = 1, [double]$dim = 1.0) {
  $s = Scale $h; $r = (Find-Element $h 'Track lanes').Current.BoundingRectangle
  $x0 = [int]($r.X + (12 + $fromMs / 1000.0 * 20) * $s); $x1 = [int]($r.X + (12 + $toMs / 1000.0 * 20) * $s)
  $y0 = [int]($r.Y + ($track * $TrackHeight + 5 + 17) * $s); $hh = [int](($TrackHeight - 10 - 18) * $s)
  Assert-CutCapForeground
  $bmp = New-Object System.Drawing.Bitmap ($x1 - $x0), $hh; $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($x0, $y0, 0, 0, (New-Object System.Drawing.Size ($x1 - $x0), $hh)); $g.Dispose()
  $wave = 0; $navy = 0
  for ($x = 0; $x -lt $bmp.Width; $x += 1) { for ($y = 0; $y -lt $bmp.Height; $y += 2) {
      $c = $bmp.GetPixel($x, $y)
      if ($c.B -gt 220 * $dim -and $c.G -gt 180 * $dim -and $c.R -gt 140 * $dim) { $wave++ } elseif ($c.B -gt 100 -and $c.R -lt 60) { $navy++ } } }
  $bmp.Dispose()
  @{ Wave = $wave; Navy = $navy }
}
function Clips-In-File {
  $doc = Get-Content -LiteralPath $project -Raw -Encoding utf8 | ConvertFrom-Json
  @($doc.project.clips | ForEach-Object { "$($_.stream.kind)@$($_.start)+$($_.duration)/in$($_.source_in.ticks)/link$(if ($_.link) { $_.link.Substring(0, 8) })" })
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

# Recorder with per-frequency levels (10 ms windows).
function Start-ToneRecorder([string]$name, [double]$seconds, [string]$tones) {
  Start-Process $loopback -ArgumentList ('"' + "$Shots\$name.wav" + '" ' + $seconds + ' 2000 --tones ' + $tones) -RedirectStandardOutput "$Shots\$name.json" -PassThru -NoNewWindow
}
# Mean amplitude of a tone between two Unix times.
function Tone-Mean($rec, [string]$hz, [double]$from, [double]$to) {
  $times = $rec.tone_times_unix_ms; $levels = $rec.tones.$hz
  $sum = 0.0; $n = 0
  for ($i = 0; $i -lt $times.Count; $i++) { if ($times[$i] -ge $from -and $times[$i] -lt $to) { $sum += $levels[$i]; $n++ } }
  if ($n -eq 0) { return 0.0 }
  [Math]::Round($sum / $n, 5)
}
# First time after $from a tone's level crosses $threshold (rising or falling).
# First 10 ms window at or past the threshold that stays there for $hold windows.
function Tone-Cross($rec, [string]$hz, [double]$from, [double]$threshold, [bool]$rising, [double]$limitMs = 3000, [int]$hold = 1) {
  $times = $rec.tone_times_unix_ms; $levels = $rec.tones.$hz
  for ($i = 0; $i -lt $times.Count; $i++) {
    if ($times[$i] -lt $from) { continue }
    if ($times[$i] -gt $from + $limitMs) { break }
    $held = $true
    for ($j = $i; $j -lt [Math]::Min($i + $hold, $times.Count); $j++) {
      if (-not (($rising -and $levels[$j] -ge $threshold) -or (-not $rising -and $levels[$j] -le $threshold))) { $held = $false; break }
    }
    if ($held) { return $times[$i] }
  }
  [double]::NaN
}
# A real mouse drag of a track's volume thumb. Returns the Unix time of the last move.
function Drag-Gain([IntPtr]$h, [string]$track, [double]$fromDb, [double]$toDb, [int]$steps = 12) {
  $r = (Find-Element $h "$track volume").Current.BoundingRectangle
  $s = Scale $h
  $thumb = 20 * $s
  $x = { param($db) [int]($r.X + $thumb / 2 + ($db + 60) / 66 * ($r.Width - $thumb)) }
  $y = [int]($r.Y + $r.Height / 2)
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos((& $x $fromDb), $y); Start-Sleep -Milliseconds 100
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 80
  for ($i = 1; $i -le $steps; $i++) {
    $db = $fromDb + ($toDb - $fromDb) * $i / $steps
    [void][Native2]::SetCursorPos((& $x $db), $y); Start-Sleep -Milliseconds 15
  }
  $t = UnixMs
  Start-Sleep -Milliseconds 60
  [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 150
  $t
}
function Gain-Text([IntPtr]$h, [string]$track) { (Find-Element $h "$track volume").Current.HelpText }
function Undo-Tip([IntPtr]$h) { (Find-Element $h 'Undo').Current.HelpText }
function Db([double]$ratio) { if ($ratio -le 0) { 'silent' } else { [Math]::Round(20 * [Math]::Log10($ratio), 2) } }

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
