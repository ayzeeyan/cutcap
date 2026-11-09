# Phase 5A: how quickly the app's UI thread answers during a real export, using
# WM_NULL round trips (SendMessageTimeout), measured idle and while exporting.
# Dot-sourced by export-bench.ps1 through filmstrip-ui.ps1 -Perf -Stage.
. "$PSScriptRoot\av-common.ps1"
if (-not ('WinPing' -as [type])) {
  Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class WinPing {
  [DllImport("user32.dll", SetLastError = true)]
  static extern IntPtr SendMessageTimeoutW(IntPtr h, uint m, IntPtr w, IntPtr l, uint f, uint t, out IntPtr r);
  public static double Ms(IntPtr h) {
    var w = System.Diagnostics.Stopwatch.StartNew();
    IntPtr r;
    var ok = SendMessageTimeoutW(h, 0, IntPtr.Zero, IntPtr.Zero, 2, 2000, out r);
    return ok == IntPtr.Zero ? -1 : w.Elapsed.TotalMilliseconds;
  }
}
"@
}
function Latency($values) {
  $s = @($values | Sort-Object)
  [ordered]@{ n = $s.Count; median = [Math]::Round($s[[int]($s.Count / 2)], 2); p95 = [Math]::Round($s[[int]($s.Count * 0.95)], 2); p99 = [Math]::Round($s[[int]($s.Count * 0.99)], 2); max = [Math]::Round($s[-1], 2); unanswered = @($s | Where-Object { $_ -lt 0 }).Count }
}
function Sample-Ping([IntPtr]$h, [int]$ms) {
  $w = [Diagnostics.Stopwatch]::StartNew(); $v = [System.Collections.Generic.List[double]]::new()
  while ($w.ElapsedMilliseconds -lt $ms) { $v.Add([WinPing]::Ms($h)); Start-Sleep -Milliseconds 20 }
  , $v.ToArray()
}
$name = 'av long 1080p.mp4'
Copy-Item (Join-Path $repo ".artifacts\av\sources\$name") (Join-Path $work $name)
$script:launch = 1
$app = Start-App; $h = $app.H
Import $h $app.P.Id (Join-Path $work $name)
Start-Sleep -Milliseconds 800
Click-Named $h 'Add to timeline'; Start-Sleep -Milliseconds 300
Note "placed: $(Wait-Lanes $h '2 clips' 15000)"
Click-Named $h 'Media collections'; Start-Sleep -Milliseconds 3000
$idle = Latency (Sample-Ping $h 5000)
Note "idle UI ping: median $($idle.median) ms, p99 $($idle.p99) ms, max $($idle.max) ms"
Click-Named $h 'Export'; Start-Sleep -Milliseconds 700
$A = [System.Windows.Automation.AutomationElement]; $T = [System.Windows.Automation.TreeScope]
$dlg = @($A::FromHandle($h).FindAll($T::Children, [System.Windows.Automation.Condition]::TrueCondition) | Where-Object { $_.Current.Name -eq 'Export — CutCap' })[0]
$x = [IntPtr]$dlg.Current.NativeWindowHandle
Click-Named $x 'Choose output file'; Start-Sleep -Milliseconds 900
Add-Type -AssemblyName System.Windows.Forms
Assert-CutCapForeground
[System.Windows.Forms.SendKeys]::SendWait('%n'); Start-Sleep -Milliseconds 200; Assert-CutCapForeground
$file = Join-Path $work 'ui bench.mp4'
[System.Windows.Forms.SendKeys]::SendWait("^a$($file -replace '([+^%~(){}\[\]])', '{$1}'){ENTER}")
Start-Sleep -Milliseconds 700
Click-Named $x 'Start export'
$w = [Diagnostics.Stopwatch]::StartNew()
$pings = [System.Collections.Generic.List[double]]::new(); $app_cpu0 = $app.P.TotalProcessorTime.TotalSeconds; $t0 = [Diagnostics.Stopwatch]::StartNew()
$peakWs = 0; $peakPrivate = 0; $children = 0
while ($w.ElapsedMilliseconds -lt 600000 -and -not (Test-Path -LiteralPath $file)) {
  $pings.Add([WinPing]::Ms($h)); $app.P.Refresh()
  $peakWs = [Math]::Max($peakWs, $app.P.WorkingSet64)
  $children = [Math]::Max($children, @(Get-Process ffmpeg -ErrorAction SilentlyContinue).Count)
  Start-Sleep -Milliseconds 20
}
$seconds = $t0.Elapsed.TotalSeconds
$app.P.Refresh()
$during = Latency $pings
Note "during a 180 s export: median $($during.median) ms, p95 $($during.p95) ms, p99 $($during.p99) ms, max $($during.max) ms, unanswered $($during.unanswered)"
$result = [ordered]@{ idle = $idle; during_export = $during; export_wall_s = [Math]::Round($seconds, 1); app_cores_during = [Math]::Round(($app.P.TotalProcessorTime.TotalSeconds - $app_cpu0) / $seconds, 2); app_working_set_peak_mib = [Math]::Round($peakWs / 1MB, 0); ffmpeg_children_peak = $children }
Save-Client $x "$Shots\responsiveness-finished.jpg"
$benchFile = Join-Path $Shots 'export-bench.json'
$bench = Get-Content -LiteralPath $benchFile -Raw | ConvertFrom-Json
$bench.responsiveness = $result
$bench | ConvertTo-Json -Depth 10 | Set-Content $benchFile -Encoding utf8
Click-Named $x 'Close export window'; Start-Sleep -Milliseconds 300
$null = Close-Discard $app 'close after responsiveness'
