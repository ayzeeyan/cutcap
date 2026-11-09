# Phase 5A export measurements on this machine, release build. Two parts:
#   1. Scenarios through the real session, with process memory and CPU sampled
#      around them from outside (CutCap's test host and every FFmpeg child).
#   2. A real export in the Release app while the UI thread's response time is
#      sampled, compared with the same app idle.
# Needs .artifacts/av/sources from the Phase 4B bench. Part 2 needs CutCap in the
# foreground while it clicks.
param([string]$Shots = '.artifacts/export/bench', [string]$Exe, [switch]$SkipUi)
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$out = Join-Path $root $Shots
New-Item -ItemType Directory -Force $out | Out-Null
$sources = Join-Path $root '.artifacts\av\sources'
$tone = Join-Path $sources 'bench tone.m4a'
if (-not (Test-Path -LiteralPath $tone)) {
  & ffmpeg -hide_banner -v error -y -f lavfi -i 'sine=frequency=700:sample_rate=48000:duration=40,volume=0.8' -c:a aac -b:a 128k $tone
}
$marks = Join-Path $out 'marks.txt'
Remove-Item -LiteralPath $marks -ErrorAction SilentlyContinue
$env:CUTCAP_BENCH_MARKS = $marks
$env:CUTCAP_BENCH_SOURCES = $sources
Push-Location $root
& cargo test --release -p cutcap-ffi --test export_bench --no-run 2>&1 | Select-Object -Last 1
$output = Join-Path $out 'bench-output.txt'
$host_ = $null
$job = Start-Process -FilePath 'cargo' -ArgumentList 'test --release -p cutcap-ffi --test export_bench -- --ignored --nocapture --test-threads=1' -NoNewWindow -PassThru -RedirectStandardOutput $output -RedirectStandardError (Join-Path $out 'bench-err.txt')
Pop-Location
$cores = [Environment]::ProcessorCount
$samples = [System.Collections.Generic.List[object]]::new()
$cpu = @{}
$last = [Diagnostics.Stopwatch]::StartNew(); $prev = 0.0
while (-not $job.HasExited) {
  $now = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  $hostP = @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -like 'export_bench*' })
  $ff = @(Get-Process ffmpeg -ErrorAction SilentlyContinue)
  $seconds = $last.Elapsed.TotalSeconds
  $sum = @{ h = 0.0; f = 0.0 }
  foreach ($p in ($hostP + $ff)) {
    $was = if ($cpu.ContainsKey($p.Id)) { $cpu[$p.Id] } else { $p.TotalProcessorTime.TotalSeconds }
    $delta = $p.TotalProcessorTime.TotalSeconds - $was
    $cpu[$p.Id] = $p.TotalProcessorTime.TotalSeconds
    if ($p.ProcessName -eq 'ffmpeg') { $sum.f += $delta } else { $sum.h += $delta }
  }
  $dt = [Math]::Max($seconds - $prev, 0.001); $prev = $seconds
  $samples.Add([pscustomobject]@{
      t = $now; children = $ff.Count
      host_ws = (($hostP | Measure-Object WorkingSet64 -Sum).Sum) / 1MB
      child_ws = (($ff | Measure-Object WorkingSet64 -Sum).Sum) / 1MB
      host_cores = $sum.h / $dt; child_cores = $sum.f / $dt
    })
  Start-Sleep -Milliseconds 200
}
$text = Get-Content -LiteralPath $output -Raw
if ($text -notmatch 'BENCH_JSON (\{.*\})') { throw "the bench did not report: $($text.Substring([Math]::Max(0, $text.Length - 800)))" }
$report = $Matches[1] | ConvertFrom-Json
$windows = @{}
foreach ($m in Get-Content -LiteralPath $marks) { $f = $m -split ' '; if (-not $windows[$f[0]]) { $windows[$f[0]] = @{} }; $windows[$f[0]][$f[1]] = [long]$f[2] }
foreach ($row in $report.scenarios) {
  $w = $windows[$row.name]
  $inside = @($samples | Where-Object { $_.t -ge $w.start -and $_.t -le $w.stop })
  $add = @{
    children_peak = ($inside | Measure-Object children -Maximum).Maximum
    host_working_set_peak_mib = [Math]::Round(($inside | Measure-Object host_ws -Maximum).Maximum, 0)
    children_working_set_peak_mib = [Math]::Round(($inside | Measure-Object child_ws -Maximum).Maximum, 0)
    host_cores_mean = [Math]::Round(($inside | Measure-Object host_cores -Average).Average, 2)
    children_cores_mean = [Math]::Round(($inside | Measure-Object child_cores -Average).Average, 2)
  }
  foreach ($k in $add.Keys) { $row | Add-Member -NotePropertyName $k -NotePropertyValue $add[$k] }
}
$bench = [ordered]@{
  generated_at = (Get-Date).ToString('o')
  machine = [ordered]@{ os = [Environment]::OSVersion.VersionString; cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name; logical_cores = $cores; memory_gib = [Math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1) }
  build = 'release engine, run through the session API'
  report = $report
  responsiveness = $null
}
$bench | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $out 'export-bench.json') -Encoding utf8
Write-Output 'part 1 done'
if ($SkipUi) { return }
$splat = @{ Shots = $Shots; Perf = $true; Stage = 'export-responsiveness.ps1'; Width = 1800; Height = 1050 }
if ($Exe) { $splat.Exe = $Exe }
$started = Get-Date
try { & "$PSScriptRoot\filmstrip-ui.ps1" @splat } finally {
  Get-Process CutCap -ErrorAction SilentlyContinue | Where-Object { $_.StartTime -ge $started } | Stop-Process -Force
}
