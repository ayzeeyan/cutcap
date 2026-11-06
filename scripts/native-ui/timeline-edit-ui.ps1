# Phase 3E native timeline-editing scenario over real input: drag from the
# library, move (within/between tracks), trim both edges, split at the
# playhead, undo/redo, snapping, an invalid overlapping drop, timeline
# playback across the new cut, save/close/reopen with exact identities and
# source ranges, source playback, and close during decode. Input is sent only
# while CutCap owns the foreground (ui.ps1 aborts otherwise). Needs the
# generated playback sources (.artifacts/playback/sources).
param(
  [string]$Shots = '.artifacts/timeline-edit/ui',
  [string]$Exe,
  [int]$Width = 1800, [int]$Height = 1050)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\ui.ps1"
. "$PSScriptRoot\find.ps1"
if (-not ('Dpi3E' -as [type])) {
  Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Dpi3E { [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h); }
"@
}
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Exe) { $Exe = "$repo\apps\CutCap.Desktop\bin\Debug\net10.0\win-x64\CutCap.exe" }
New-Item -ItemType Directory -Force $Shots | Out-Null
$Shots = (Resolve-Path $Shots).Path
$work = Join-Path $repo '.artifacts\timeline-edit\work'
if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
New-Item -ItemType Directory -Force $work | Out-Null
$interview = Join-Path $work 'interview take.mp4'
$colors = Join-Path $work 'colors 视频.mkv'
Copy-Item "$repo\.artifacts\playback\sources\long-1080p.mp4" $interview
Copy-Item "$repo\tests\fixtures\colors-cfr.mkv" $colors
$project = Join-Path $work 'advanced edit.cutcap'

$log = [System.Collections.Generic.List[string]]::new()
function Note([string]$text) { $line = "{0:HH:mm:ss.fff} {1}" -f (Get-Date), $text; $log.Add($line); Write-Output $line }
function Lanes([IntPtr]$h) { (Find-Element $h 'Track lanes').Current.Name }
function ById([IntPtr]$h, [string]$id) {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $id)
  ([System.Windows.Automation.AutomationElement]::FromHandle($h)).FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c).Current.Name
}
function Playhead([IntPtr]$h) { ById $h 'TimelinePlayhead' }
function Hint([IntPtr]$h) {
  $id = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'TimelineEditHint')
  ([System.Windows.Automation.AutomationElement]::FromHandle($h)).FindFirst([System.Windows.Automation.TreeScope]::Descendants, $id).Current.Name
}
function Wait-Lanes([IntPtr]$h, [string]$fragment, [int]$ms = 8000) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  while ($w.ElapsedMilliseconds -lt $ms) { $n = Lanes $h; if ($n.Contains($fragment)) { return "$($w.ElapsedMilliseconds) ms" }; Start-Sleep -Milliseconds 60 }
  throw "lanes did not show '$fragment' within $ms ms (now '$(Lanes $h)')"
}
function Enabled([IntPtr]$h, [string]$name) { (Find-Element $h $name).Current.IsEnabled }
function Help([IntPtr]$h, [string]$name) { (Find-Element $h $name).Current.HelpText }
function Ticks([IntPtr]$h) { Read-Text $h '@ 1/720000' }
function SourceIn([IntPtr]$h) { (All-Elements $h | Where-Object { $_.Current.Name -like 'PTS * @ *' -and (Visible $_) } | Select-Object -First 1).Current.Name }
function Status([IntPtr]$h) { (All-Elements $h | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Text' -and (Visible $_) } | Select-Object -Skip 2 -First 1).Current.Name }
function State([IntPtr]$h, [string]$label) {
  Note "$label | lanes='$(Lanes $h)' ticks='$(Ticks $h)' undo=$(Enabled $h 'Undo') redo=$(Enabled $h 'Redo') split=$(Enabled $h 'Split clip') title='$((Get-Process -Id $script:CutCapPid).MainWindowTitle)'"
}
function Scale([IntPtr]$h) { [Dpi3E]::GetDpiForWindow($h) / 96.0 }
# A physical screen point for a timeline position (ms) and track lane at a zoom (no horizontal scroll).
function Lane-Point([IntPtr]$h, [double]$ms, [int]$track, [double]$dy = 0, [int]$zoom = 100) {
  $s = Scale $h; $r = (Find-Element $h 'Track lanes').Current.BoundingRectangle
  @{ X = [int]($r.X + (12 + $ms / 1000.0 * 20 * $zoom / 100) * $s); Y = [int]($r.Y + ($track * 50 + 25 + $dy) * $s) }
}
function Ruler-Point([IntPtr]$h, [double]$ms, [int]$zoom = 100) {
  $s = Scale $h; $r = (Find-Element $h 'Timeline ruler').Current.BoundingRectangle
  @{ X = [int]($r.X + (12 + $ms / 1000.0 * 20 * $zoom / 100) * $s); Y = [int]($r.Y + $r.Height / 2) }
}
function Click-Point($p) {
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos($p.X, $p.Y); Start-Sleep -Milliseconds 80
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40; [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 350
}
# A real mouse drag in physical pixels, in small steps (OLE drag-and-drop needs motion).
function Drag-Points($from, $to, [int]$steps = 24, [int]$pause = 25, [string]$holdKey = '') {
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos($from.X, $from.Y); Start-Sleep -Milliseconds 120
  if ($holdKey) { [Native2]::keybd_event([byte]$script:VK[$holdKey], 0, 0, [IntPtr]::Zero) }
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 120
  for ($i = 1; $i -le $steps; $i++) {
    Assert-CutCapForeground
    [void][Native2]::SetCursorPos([int]($from.X + ($to.X - $from.X) * $i / $steps), [int]($from.Y + ($to.Y - $from.Y) * $i / $steps))
    Start-Sleep -Milliseconds $pause
  }
  Start-Sleep -Milliseconds 250
  $hint = Hint $h
  [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  if ($holdKey) { [Native2]::keybd_event([byte]$script:VK[$holdKey], 0, 2, [IntPtr]::Zero) }
  Start-Sleep -Milliseconds 600
  $hint
}
function Complete-Dialog([int]$ProcessId, [string]$title, [string]$path) {
  $A = [System.Windows.Automation.AutomationElement]; $T = [System.Windows.Automation.TreeScope]
  $main = $A::FromHandle((Get-CutCapWindow $ProcessId))
  $dlg = $null
  for ($i = 0; $i -lt 50 -and -not $dlg; $i++) {
    $dlg = $main.FindAll($T::Children, [System.Windows.Automation.Condition]::TrueCondition) | Where-Object { $_.Current.Name -eq $title } | Select-Object -First 1
    if (-not $dlg) { Start-Sleep -Milliseconds 150 }
  }
  if (-not $dlg) { throw "ABORT: dialog '$title' not found" }
  Start-Sleep -Milliseconds 300
  Assert-CutCapForeground
  Add-Type -AssemblyName System.Windows.Forms
  [System.Windows.Forms.SendKeys]::SendWait('%n'); Start-Sleep -Milliseconds 200
  Assert-CutCapForeground
  [System.Windows.Forms.SendKeys]::SendWait("^a$($path -replace '([+^%~(){}\[\]])', '{$1}'){ENTER}")
}
function Import([IntPtr]$h, [int]$ProcessId, [string]$path) {
  Send-Key $h 'CTRL+I'; Start-Sleep -Milliseconds 700
  Complete-Dialog $ProcessId 'Import source video' $path; Start-Sleep -Milliseconds 2500
}
function Open-Project([IntPtr]$h, [int]$ProcessId) {
  Send-Key $h 'CTRL+O'; Start-Sleep -Milliseconds 900
  Complete-Dialog $ProcessId 'Open CutCap project' $project
  for ($i = 0; $i -lt 20; $i++) {
    if (@(All-Elements $h | Where-Object { $_.Current.Name -eq 'Continue without saving' -and (Visible $_) }).Count) {
      Note 'unsaved prompt for the empty untitled project; continuing without saving'; Click-Named $h 'Continue without saving'; return
    }
    if ((Lanes $h) -notmatch ', 0 clips') { return }
    Start-Sleep -Milliseconds 250
  }
}
# Filters the library so the tile is on screen; returns its centre.
function Tile([IntPtr]$h, [string]$fragment, [string]$name) {
  Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Type-Text $h $fragment; Start-Sleep -Milliseconds 500
  $r = (Find-Element $h $name).Current.BoundingRectangle
  @{ X = [int]($r.X + $r.Width / 2); Y = [int]($r.Y + $r.Height / 3) }
}
function Clear-Search([IntPtr]$h) { Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Send-Key $h 'BACK'; Start-Sleep -Milliseconds 400 }
function Start-App {
  $p = Start-Process $Exe -WorkingDirectory $repo -PassThru -RedirectStandardError "$Shots\app-err-$($script:launch).txt" -RedirectStandardOutput "$Shots\app-out-$($script:launch).txt"
  $script:launch++
  $script:CutCapPid = $p.Id
  $h = Get-CutCapWindow $p.Id
  Start-Sleep -Milliseconds 1500
  [void][Native2]::MoveWindow($h, 40, 20, $Width, $Height, $true)
  Start-Sleep -Milliseconds 700
  @{ P = $p; H = $h }
}
function Close-App($app, [string]$label) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  [void]$app.P.CloseMainWindow()
  $exited = $app.P.WaitForExit(10000)
  Start-Sleep -Milliseconds 500
  $left = @(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count
  Note "$label | exited=$exited in $($w.ElapsedMilliseconds) ms code=$(if ($exited) { $app.P.ExitCode } else { 'n/a' }) leftover=$left"
}
function Clip-Rows {
  $doc = Get-Content -LiteralPath $project -Raw -Encoding utf8 | ConvertFrom-Json
  @($doc.project.clips | ForEach-Object { "$($_.id)|$($_.track_id)|$($_.start)|$($_.duration)|$($_.source_in.ticks)" })
}
$script:launch = 1

$app = Start-App; $h = $app.H
Import $h $app.P.Id $interview
Import $h $app.P.Id $colors
State $h 'imported two sources'

# 1-2. Drag the 20 s interview from the library onto Video 1 at about 2 s.
$from = Tile $h 'interview' 'interview take.mp4'
Note "drag interview -> V1 @2s: hint at release='$(Drag-Points $from (Lane-Point $h 2000 0) 30 30)'"
Note "placed: $(Wait-Lanes $h '1 clip')"
Clear-Search $h
State $h 'dropped on Video 1'
Save-Client $h "$Shots\01-dropped.jpg"

# 3. Move the clip body 3 s later (one gesture, one undo step).
Note "move body +3 s: hint='$(Drag-Points (Lane-Point $h 9000 0) (Lane-Point $h 12000 0))'"
Note "moved: $(Wait-Lanes $h '00:05.000 to 00:25.000')"
State $h 'moved within Video 1'

# 4-5. Add a video track and move the clip onto it.
Click-Named $h 'Add video track'; Start-Sleep -Milliseconds 800
Note "move to Video 2: hint='$(Drag-Points (Lane-Point $h 12000 0) (Lane-Point $h 12000 2))'"
Start-Sleep -Milliseconds 600
Note "inspector after cross-track move: '$(Read-Text $h 'Video clip on')'"
State $h 'moved to Video 2'
Save-Client $h "$Shots\02-moved-to-v2.jpg"

# 6. Trim the left edge 2 s later; 7. trim the right edge 4 s earlier.
$s0 = Lane-Point $h 5000 2; $s0.X += [int](3 * (Scale $h))
Note "trim start +2 s: hint='$(Drag-Points $s0 (Lane-Point $h 7000 2) 20 30)'"
Note "start trimmed: $(Wait-Lanes $h '00:07.000 to 00:25.000')"
$e0 = Lane-Point $h 25000 2; $e0.X -= [int](3 * (Scale $h))
Note "trim end -4 s: hint='$(Drag-Points $e0 (Lane-Point $h 21000 2) 20 30)'"
Note "end trimmed: $(Wait-Lanes $h '00:07.000 to 00:21.000')"
State $h 'trimmed both edges'
Note "source in after trims: '$(SourceIn $h)'"
Save-Client $h "$Shots\03-trimmed.jpg"

# 8. Seek the playhead to 14 s (ruler) and split with Ctrl+B.
Click-Point (Ruler-Point $h 14000); Start-Sleep -Milliseconds 1200
Click-Point (Lane-Point $h 10000 2)
Note "split tip: '$(Help $h 'Split clip')' enabled=$(Enabled $h 'Split clip')"
Click-Named $h 'Media collections'
Send-Key $h 'CTRL+B'; Note "split: $(Wait-Lanes $h '2 clips')"
State $h 'split at the playhead'
Save-Client $h "$Shots\04-split.jpg"
# 9-10. Undo the split (one step), redo it.
Send-Key $h 'CTRL+Z'; Note "undo split: $(Wait-Lanes $h '1 clip')"
State $h 'undo split'
Send-Key $h 'CTRL+Y'; Note "redo split: $(Wait-Lanes $h '2 clips')"
State $h 'redo split'

# 11. Snapping: drop the colors clip near the right part's end (21 s) on Video 2.
$from = Tile $h 'colors' 'colors 视频.mkv'
Note "drop colors near 21 s on V2: hint='$(Drag-Points $from (Lane-Point $h 21150 2) 30 30)'"
Note "snapped drop: $(Wait-Lanes $h '3 clips')"
Clear-Search $h
State $h 'snapped drop (selected = colors)'
Save-Client $h "$Shots\05-snapped.jpg"

# 12. Invalid overlapping drop onto the middle of the right part: nothing changes.
$from = Tile $h 'colors' 'colors 视频.mkv'
$hint = Drag-Points $from (Lane-Point $h 17000 2) 30 30
Clear-Search $h
Note "overlapping drop: hint='$hint' lanes='$(Lanes $h)'"
# Invalid move: drag the colors clip onto the audio lane.
Note "move onto audio lane: hint='$(Drag-Points (Lane-Point $h 22500 2) (Lane-Point $h 22500 1))' lanes='$(Lanes $h)'"
# Escape cancels a drag in progress.
Assert-CutCapForeground
$p = Lane-Point $h 22500 2; [void][Native2]::SetCursorPos($p.X, $p.Y); Start-Sleep -Milliseconds 100
[Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 100
foreach ($i in 1..10) { [void][Native2]::SetCursorPos($p.X + $i * 12, $p.Y); Start-Sleep -Milliseconds 30 }
Save-Client $h "$Shots\06-drag-ghost.jpg"
Note "mid-drag hint='$(Hint $h)'"
Send-Key $h 'ESC'; Start-Sleep -Milliseconds 200
[Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 500
State $h 'escape cancelled the drag'

# 13-14. Play the edited timeline on Video 2 from 6 s, then seek across the cut while playing.
Click-Point (Lane-Point $h 10000 2)
Click-Point (Ruler-Point $h 6000); Start-Sleep -Milliseconds 1200
Click-Named $h 'Play timeline'; Start-Sleep -Milliseconds 2500
Note "timeline playing: play button='$(@(All-Elements $h | Where-Object { $_.Current.Name -like '*timeline' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' } | ForEach-Object { $_.Current.Name }) -join ',')' playhead='$(Playhead $h)'"
Save-Client $h "$Shots\07-playing-edited.jpg"
Click-Point (Ruler-Point $h 13500); Start-Sleep -Milliseconds 2500
Note "seek across the cut while playing: playhead='$(Playhead $h)'"
Save-Client $h "$Shots\08-across-cut.jpg"
Click-Named $h 'Pause timeline'; Start-Sleep -Milliseconds 400

# 15. Save.
Send-Key $h 'CTRL+SHIFT+S'; Start-Sleep -Milliseconds 900
Complete-Dialog $app.P.Id 'Save CutCap project' $project; Start-Sleep -Milliseconds 2000
State $h 'saved'
$saved = Clip-Rows
Note "saved clips: $($saved -join ' ; ')"
Save-Client $h "$Shots\09-saved.jpg"
[void][Native2]::MoveWindow($h, 40, 20, 1440, 900, $true); Start-Sleep -Milliseconds 900
Save-Client $h "$Shots\10-minimum-window.jpg"
Close-App $app 'close after save'

# 16-17. Reopen; identities, positions and source ranges must match the file and the UI.
$app = Start-App; $h = $app.H
Open-Project $h $app.P.Id
Note "reopened: $(Wait-Lanes $h '3 clips' 15000)"
Start-Sleep -Milliseconds 2500
foreach ($ms in 9000, 16000, 22000) {
  Click-Point (Lane-Point $h $ms 2)
  Note "clip at $ms ms: ticks='$(Ticks $h)' source='$(SourceIn $h)'"
}
$doc = Get-Content -LiteralPath $project -Raw -Encoding utf8 | ConvertFrom-Json
Note "file clips after reopen (unchanged file): $((Clip-Rows) -join ' ; ') identical=$(((Clip-Rows) -join ';') -eq ($saved -join ';'))"
State $h 'reopened'
Save-Client $h "$Shots\11-reopened.jpg"

# 18. Source playback is independent; 19-20. close during active decode.
$t = Tile $h 'interview' 'interview take.mp4'; Click-Point $t; Clear-Search $h
Start-Sleep -Milliseconds 1500
Click-Named $h 'Play'; Start-Sleep -Milliseconds 1500
Note "source playing: button='$(@(All-Elements $h | Where-Object { $_.Current.Name -in 'Play','Pause' -and (Visible $_) } | ForEach-Object { $_.Current.Name }) -join ',')' lanes='$(Lanes $h)'"
Save-Client $h "$Shots\12-source-playing.jpg"
Close-App $app 'close during active source decode'
$log | Set-Content "$Shots\scenario.txt" -Encoding utf8
