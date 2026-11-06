# Phase 3B draw-time check: opens a generated many-clip project (see
# generate_large_timeline_project in crates/cutcap-ffi/src/timeline_tests.rs)
# with CUTCAP_TRACE_TIMELINE=1, then zooms and scrolls the lanes. Input is sent
# only while CutCap owns the foreground (ui.ps1 aborts otherwise).
param(
  [string]$Project = '.artifacts/timeline/large/large timeline.cutcap',
  [string]$Out = '.artifacts/timeline/draw',
  [string]$Exe)
$ErrorActionPreference = 'Stop'
if (-not ('Dpi.Window' -as [type])) { Add-Type -Namespace Dpi -Name Window -MemberDefinition '[DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr h); public static double Scale(IntPtr h) { return GetDpiForWindow(h) / 96.0; }' }
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
# The untitled start-up project is dirty, so Open asks first; the prompt can
# arrive several seconds after the file is chosen while UIA is slow.
while ((Lanes $h) -notmatch '5000 clips') {
  if (@(All-Elements $h | Where-Object { $_.Current.Name -eq 'Continue without saving' -and (Visible $_) }).Count) { Click-Named $h 'Continue without saving' }
  if ($watch.ElapsedMilliseconds -gt 60000) { throw "large project not shown: $(Lanes $h)" }
  Start-Sleep -Milliseconds 250
}
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
# Phase 3E: a clip drag (ghost preview, snapping and overlap lookups per move),
# released on top of a neighbour so it is rejected locally and nothing is edited.
Click-Named $h 'Reset zoom'; Start-Sleep -Milliseconds 400
$s = [Dpi.Window]::Scale($h)
$lanes = (Find-Element $h 'Track lanes').Current.BoundingRectangle
$from = @{ X = [int]($lanes.X + (12 + 4.5 * 20) * $s); Y = [int]($lanes.Y + 25 * $s) }
Assert-CutCapForeground
[void][Native2]::SetCursorPos($from.X, $from.Y); Start-Sleep -Milliseconds 100
[Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 100
$moveWatch = [Diagnostics.Stopwatch]::StartNew()
for ($i = 1; $i -le 90; $i++) {
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos($from.X + [int]($i * 4 * $s), $from.Y); Start-Sleep -Milliseconds 16
}
"drag: 90 pointer moves in $($moveWatch.ElapsedMilliseconds) ms (incl. 16 ms sleeps)"
Send-Key $h 'ESC'; Start-Sleep -Milliseconds 200
[Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 800
"after cancelled drag: $(Lanes $h)"
[void]$p.CloseMainWindow(); [void]$p.WaitForExit(10000)
"exited=$($p.HasExited)"
Get-Content "$Out\trace.txt"
