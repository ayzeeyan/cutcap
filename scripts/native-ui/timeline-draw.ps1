# Phase 3B draw-time check: opens a generated many-clip project (see
# generate_large_timeline_project in crates/cutcap-ffi/src/timeline_tests.rs)
# with CUTCAP_TRACE_TIMELINE=1, then zooms and scrolls the lanes. Input is sent
# only while CutCap owns the foreground (ui.ps1 aborts otherwise).
param(
  [string]$Project = '.artifacts/timeline/large/large timeline.cutcap',
  [string]$Out = '.artifacts/timeline/draw',
  [string]$Exe)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\ui.ps1"
. "$PSScriptRoot\find.ps1"
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Exe) { $Exe = "$repo\apps\CutCap.Desktop\bin\Debug\net10.0\win-x64\CutCap.exe" }
$Project = (Resolve-Path -LiteralPath $Project).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
function Lanes([IntPtr]$h) { (Find-Element $h 'Track lanes').Current.Name }

$env:CUTCAP_TRACE_TIMELINE = '1'
$p = Start-Process $Exe -WorkingDirectory $repo -PassThru -RedirectStandardOutput "$Out\trace.txt" -RedirectStandardError "$Out\err.txt"
$env:CUTCAP_TRACE_TIMELINE = $null
$script:CutCapPid = $p.Id
$h = Get-CutCapWindow $p.Id
Start-Sleep -Milliseconds 1500
[void][Native2]::MoveWindow($h, 40, 20, 1800, 1050, $true); Start-Sleep -Milliseconds 700

Send-Key $h 'CTRL+O'; Start-Sleep -Milliseconds 900
$main = [System.Windows.Automation.AutomationElement]::FromHandle($h)
for ($i = 0; $i -lt 50; $i++) {
  if ($main.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition) | Where-Object { $_.Current.Name -eq 'Open CutCap project' }) { break }
  Start-Sleep -Milliseconds 150
}
Assert-CutCapForeground
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait('%n'); Start-Sleep -Milliseconds 200; Assert-CutCapForeground
[System.Windows.Forms.SendKeys]::SendWait("^a$($Project -replace '([+^%~(){}\[\]])', '{$1}'){ENTER}")
$watch = [Diagnostics.Stopwatch]::StartNew()
for ($i = 0; $i -lt 40; $i++) {
  if (@(All-Elements $h | Where-Object { $_.Current.Name -eq 'Continue without saving' -and (Visible $_) }).Count) { Click-Named $h 'Continue without saving'; break }
  if ((Lanes $h) -match '5000 clips') { break }
  Start-Sleep -Milliseconds 250
}
while ((Lanes $h) -notmatch '5000 clips') { if ($watch.ElapsedMilliseconds -gt 30000) { throw "large project not shown: $(Lanes $h)" }; Start-Sleep -Milliseconds 200 }
"opened: $(Lanes $h) after $($watch.ElapsedMilliseconds) ms (includes UIA polling)"
Start-Sleep -Milliseconds 1500
Save-Client $h "$Out\large-100.jpg"

# Horizontal wheel scrolling over the lanes (Shift+wheel), then zoom steps.
$r = (Find-Element $h 'Track lanes').Current.BoundingRectangle
$viewport = (Find-Element $h 'Timeline ruler').Current.BoundingRectangle
Focus-CutCap $h
[void][Native2]::SetCursorPos([int]($viewport.X + 300), [int]($r.Y + 40))
for ($i = 0; $i -lt 60; $i++) {
  Assert-CutCapForeground
  [Native2]::keybd_event(0x10, 0, 0, [IntPtr]::Zero)
  [Native2]::mouse_event(0x0800, 0, 0, -120, [IntPtr]::Zero)
  [Native2]::keybd_event(0x10, 0, 2, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 40
}
Start-Sleep -Milliseconds 500
Save-Client $h "$Out\large-scrolled.jpg"
foreach ($k in 1..4) { Click-Named $h 'Zoom in' }
foreach ($k in 1..6) { Click-Named $h 'Zoom out' }
Click-Named $h 'Reset zoom'
for ($i = 0; $i -lt 40; $i++) {
  Assert-CutCapForeground
  [Native2]::keybd_event(0x10, 0, 0, [IntPtr]::Zero)
  [Native2]::mouse_event(0x0800, 0, 0, 120, [IntPtr]::Zero)
  [Native2]::keybd_event(0x10, 0, 2, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 40
}
Start-Sleep -Milliseconds 800
[void]$p.CloseMainWindow(); [void]$p.WaitForExit(10000)
"exited=$($p.HasExited)"
Get-Content "$Out\trace.txt"
