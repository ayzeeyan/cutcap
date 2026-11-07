# Summarizes a `filmstrip-ui.ps1 -Perf` run: joins the scenario's MARK lines
# with the app's CUTCAP_TRACE_THUMBNAILS / CUTCAP_TRACE_TIMELINE output and
# writes one JSON document. Times are measured from each mark (the scripted
# input), so they include UI input latency, not only engine time.
param(
  [string]$Run = '.artifacts/filmstrip/perf',
  [string]$Out = 'docs/measurements/phase4a-filmstrip-performance.json')
$ErrorActionPreference = 'Stop'
$inv = [Globalization.CultureInfo]::InvariantCulture
function T([string]$s) { [TimeSpan]::ParseExact($s, 'hh\:mm\:ss\.fff', $inv).TotalMilliseconds }
function Field([string]$line, [string]$name) { if ($line -match "(?:^| )$name=([^ ]*)") { $Matches[1] } else { $null } }
function Num($v) { if ($null -eq $v -or $v -eq '') { $null } else { [double]::Parse($v, $inv) } }
function Stats([double[]]$xs) {
  if (-not $xs -or $xs.Count -eq 0) { return $null }
  $sorted = $xs | Sort-Object
  [ordered]@{ n = $xs.Count; avg = [Math]::Round(($xs | Measure-Object -Average).Average, 1)
    p95 = $sorted[[Math]::Min($sorted.Count - 1, [int][Math]::Ceiling(0.95 * $sorted.Count) - 1)]; max = $sorted[-1] }
}

$scenario = Get-Content (Join-Path $Run 'perf-scenario.txt')
$trace = Get-Content (Join-Path $Run 'app-out-1.txt') | Where-Object { $_ -like 'thumb at=*' } | ForEach-Object {
  [pscustomobject]@{ At = (T (Field $_ 'at')); Kind = ($_ -split ' ')[3]; Line = $_ }
}
$timeline = Get-Content (Join-Path $Run 'app-out-1.txt') | Where-Object { $_ -like 'timeline-render*' }
$marks = @($scenario | Where-Object { $_ -match '^(\S+) MARK (\S+)' } | ForEach-Object {
  $null = $_ -match '^(\S+) MARK (\S+)'; [pscustomobject]@{ Name = $Matches[2]; At = (T $Matches[1]) } })
$phases = [ordered]@{}
for ($i = 0; $i -lt $marks.Count; $i++) {
  $from = $marks[$i].At; $to = if ($i + 1 -lt $marks.Count) { $marks[$i + 1].At } else { [double]::MaxValue }
  $rows = @($trace | Where-Object { $_.At -ge $from -and $_.At -lt $to })
  $renders = @($rows | Where-Object Kind -eq 'render')
  $demands = @($rows | Where-Object { $_.Kind -eq 'demand' -and (Num (Field $_.Line 'n')) -gt 0 })
  $copies = @($rows | Where-Object Kind -eq 'copied')
  $plans = @($rows | Where-Object Kind -eq 'plan')
  $first = $renders | Where-Object { (Num (Field $_.Line 'thumbs')) -gt 0 } | Select-Object -First 1
  $missing = @($renders | Where-Object { (Num (Field $_.Line 'placeholders')) -gt 0 })
  $lastMissing = $missing | Select-Object -Last 1
  $filled = if ($lastMissing) { $renders | Where-Object { $_.At -gt $lastMissing.At -and (Num (Field $_.Line 'placeholders')) -eq 0 } | Select-Object -First 1 } else { $null }
  $playback = @($rows | Where-Object Kind -eq 'playback')
  $phase = [ordered]@{
    demands_sent = $demands.Count
    first_demand_ms = if ($demands) { [Math]::Round($demands[0].At - $from) } else { $null }
    first_thumbnail_drawn_ms = if ($first) { [Math]::Round($first.At - $from) } else { $null }
    renders_with_placeholders = $missing.Count
    max_placeholders_in_one_render = if ($missing) { ($missing | ForEach-Object { Num (Field $_.Line 'placeholders') } | Measure-Object -Maximum).Maximum } else { 0 }
    viewport_filled_ms = if ($filled) { [Math]::Round($filled.At - $from) } elseif (-not $missing -and $renders) { 0 } else { $null }
    thumbnails_copied = ($copies | ForEach-Object { Num (Field $_.Line 'n') } | Measure-Object -Sum).Sum
    copy_us_per_drain = Stats @($copies | ForEach-Object { Num (Field $_.Line 'copy_us') })
    lanes_render_record_us = Stats @($renders | ForEach-Object { Num (Field $_.Line 'draw_us') })
    plan_us = Stats @($plans | ForEach-Object { Num (Field $_.Line 'plan_us') })
    scroll_at_end = if ($renders) { Num (Field $renders[-1].Line 'scroll') } else { $null }
    zoom_at_end = if ($renders) { Num (Field $renders[-1].Line 'zoom') } else { $null }
  }
  if ($copies) {
    $a = $copies[0].Line; $b = $copies[-1].Line
    $phase.cache_hits_delta = (Num (Field $b 'hits')) - (Num (Field $a 'hits'))
    $phase.batches_at_end = Num (Field $b 'batches'); $phase.cancelled_batches_at_end = Num (Field $b 'cancelled')
    $phase.last_batch_ms = Num (Field $b 'batch_ms')
    $phase.resident_kib_at_end = Num (Field $b 'resident_kib'); $phase.entries_at_end = Num (Field $b 'entries')
    $phase.working_set_mib_max = ($copies | ForEach-Object { Num (Field $_.Line 'ws_mib') } | Measure-Object -Maximum).Maximum
  }
  $playing = @($playback | Where-Object { (Field $_.Line 'state') -eq 'Playing' })
  if ($playback) {
    $firstPlaying = $playing | Select-Object -First 1
    # Sampled once per second, so this bounds start-up from above by up to 1 s.
    $phase.starting_until_playing_ms = if ($firstPlaying) { [Math]::Round($firstPlaying.At - $playback[0].At) } else { $null }
  }
  if ($playing.Count -ge 2) {
    $a = $playing[0]; $b = $playing[-1]; $seconds = ($b.At - $a.At) / 1000
    $phase.playing_seconds_sampled = [Math]::Round($seconds, 2)
    $phase.playback_copied_fps_while_playing = [Math]::Round(((Num (Field $b.Line 'copied')) - (Num (Field $a.Line 'copied'))) / $seconds, 2)
    $phase.playback_dropped = (Num (Field $b.Line 'dropped')) - (Num (Field $a.Line 'dropped'))
    $phase.playback_stalls = (Num (Field $b.Line 'stalls')) - (Num (Field $a.Line 'stalls'))
    $phase.thumbnails_throttled = ($playback | Where-Object { (Field $_.Line 'throttled') -eq 'True' }).Count -gt 0
  }
  $phases[$marks[$i].Name] = $phase
}
$close = $scenario | Where-Object { $_ -like '*close during thumbnail decoding*' } | Select-Object -First 1
$allCopies = @($trace | Where-Object Kind -eq 'copied')
$last = $allCopies[-1].Line
$result = [ordered]@{
  generated = (Get-Date).ToString('s')
  source = 'generated testsrc2 1920x1080 30 fps H.264, GOP 250, 180 s (long 1080p.mp4)'
  scale = ($scenario | Where-Object { $_ -match 'scale=' } | Select-Object -First 1) -replace '^\S+ ', ''
  note = 'Times are from the scripted input (MARK) to the trace event, so they include UI input latency. draw_us is draw-call recording time on the UI thread, not GPU raster or presentation time.'
  phases = $phases
  totals = [ordered]@{
    ffmpeg_batches = Num (Field $last 'batches'); cancelled_batches = Num (Field $last 'cancelled')
    thumbnails_copied = ($allCopies | ForEach-Object { Num (Field $_.Line 'n') } | Measure-Object -Sum).Sum
    cache_hits = Num (Field $last 'hits'); cache_misses = Num (Field $last 'misses'); evictions = Num (Field $last 'evictions')
    resident_kib = Num (Field $last 'resident_kib'); entries = Num (Field $last 'entries')
    working_set_mib_max = ($allCopies | ForEach-Object { Num (Field $_.Line 'ws_mib') } | Measure-Object -Maximum).Maximum
    managed_mib_max = ($allCopies | ForEach-Object { Num (Field $_.Line 'managed_mib') } | Measure-Object -Maximum).Maximum
  }
  timeline_render_trace = @($timeline)
  close = $close
}
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
$result | ConvertTo-Json -Depth 6 | Set-Content $Out -Encoding utf8
Write-Host "wrote $Out"
