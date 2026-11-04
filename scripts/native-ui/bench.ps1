# Release preview benchmark over the real UI: import a synthetic 1080p clip, five go-to
# seeks, idle CPU, memory and close time. Sends input only while CutCap owns the
# foreground (ui.ps1 aborts otherwise). Generate the clip first:
#   ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 3 -c:v libx264 -pix_fmt yuv420p -g 30 .artifacts/bench/h264-1080p.mp4
# -Exe can point at another build, such as the archived Slint baseline (tracing on stdout).
param([string]$App = 'avalonia', [string]$Exe, [int]$Runs = 3, [string]$Out = '.artifacts/bench/results.json')
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\ui.ps1"
. "$PSScriptRoot\find.ps1"
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$exe = if ($Exe) { $Exe } else { "$repo\artifacts\publish\win-x64\CutCap.exe" }
$clip = "$repo\.artifacts\bench\h264-1080p.mp4"
$env:CUTCAP_TRACE_PREVIEW = '1'
$results = @()
for ($run = 1; $run -le $Runs; $run++) {
  $err = "$repo\.artifacts\bench\$App-$run.err.log"
  $watch = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process $exe -WorkingDirectory $repo -PassThru -RedirectStandardError $err -RedirectStandardOutput "$repo\.artifacts\bench\$App-$run.out.log"
  $script:CutCapPid = $p.Id
  $h = Get-CutCapWindow $p.Id
  $windowMs = $watch.ElapsedMilliseconds
  Start-Sleep -Milliseconds 2000
  [void][Native2]::MoveWindow($h, 40, 20, 1800, 1050, $true)
  Start-Sleep -Milliseconds 800
  $p.Refresh(); $startupWorkingSet = $p.WorkingSet64
  Import-Fixture $h $p.Id $clip
  Start-Sleep -Milliseconds 2500
  foreach ($ms in 500, 1500, 2500, 1000, 2000) {
    Click-Named $h 'Go to source time'
    Send-Key $h 'CTRL+A'; Type-Text $h "$ms"; Send-Key $h 'ENTER'
    Start-Sleep -Milliseconds 1500
  }
  $p.Refresh(); $cpu0 = $p.TotalProcessorTime; $idle = [Diagnostics.Stopwatch]::StartNew()
  Start-Sleep -Seconds 5
  $p.Refresh(); $idleCpu = ($p.TotalProcessorTime - $cpu0).TotalMilliseconds / $idle.Elapsed.TotalMilliseconds / [Environment]::ProcessorCount * 100
  $workingSet = $p.WorkingSet64; $private = $p.PrivateMemorySize64; $peak = $p.PeakWorkingSet64
  $close = [Diagnostics.Stopwatch]::StartNew()
  [void]$p.CloseMainWindow(); [void]$p.WaitForExit(15000)
  $closeMs = $close.ElapsedMilliseconds
  Start-Sleep -Milliseconds 300
  $leftover = @(Get-Process ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count
  $log = @(Get-Content $err) + @(Get-Content "$repo\.artifacts\bench\$App-$run.out.log")
  if ($App -eq 'slint') {
    $present = $log | Select-String 'preview presented' | ForEach-Object {
      [pscustomobject]@{ request = [int64]([regex]::Match($_, 'request_to_ui_us=(\d+)').Groups[1].Value); import = [int64]('0' + [regex]::Match($_, 'import_to_ui_us=(?:Some\()?(\d+)').Groups[1].Value); copy = [int64]([regex]::Match($_, 'copy_us=(\d+)').Groups[1].Value); decode = [int64]([regex]::Match($_, 'decode_us=(\d+)').Groups[1].Value) } }
  } else {
    $decodes = @($log | Select-String 'preview frame ready' | ForEach-Object { [int64]([regex]::Match($_, 'decode_us=(\d+)').Groups[1].Value) })
    $i = 0
    $present = $log | Select-String 'ui-present' | ForEach-Object {
      $o = [pscustomobject]@{ request = [int64]([regex]::Match($_, 'request_to_present_us=(\d+)').Groups[1].Value); import = [int64]('0' + [regex]::Match($_, 'import_to_present_us=(\d+)').Groups[1].Value); copy = [int64]([regex]::Match($_, 'copy_us=(\d+)').Groups[1].Value); decode = $(if ($i -lt $decodes.Count) { $decodes[$i] } else { 0 }) }
      $i++; $o }
  }
  $present = @($present)
  $results += [pscustomobject]@{
    app = $App; run = $run; window_ms = $windowMs; exit_code = $p.ExitCode; close_ms = $closeMs; leftover_children = $leftover
    startup_working_set_mb = [math]::Round($startupWorkingSet / 1MB, 1)
    working_set_mb = [math]::Round($workingSet / 1MB, 1); private_mb = [math]::Round($private / 1MB, 1); peak_working_set_mb = [math]::Round($peak / 1MB, 1)
    idle_cpu_percent = [math]::Round($idleCpu, 2)
    import_to_first_frame_ms = $(if ($present.Count) { [math]::Round($present[0].import / 1000, 1) } else { $null })
    seek_ms = ($present | Select-Object -Skip 1 | ForEach-Object { [math]::Round($_.request / 1000, 1) }) -join ' '
    copy_us = ($present | ForEach-Object { $_.copy }) -join ' '
    decode_ms = ($present | ForEach-Object { [math]::Round($_.decode / 1000, 1) }) -join ' '
  }
}
$results | ConvertTo-Json -Depth 3 | Set-Content $Out
$results | Format-List
