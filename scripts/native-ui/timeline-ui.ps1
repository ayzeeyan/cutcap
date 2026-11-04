# Phase 3B native timeline-editing scenario over real input. Sends input only
# while CutCap owns the foreground (ui.ps1 aborts otherwise). File dialogs are
# completed through UI Automation (value + invoke), not keystrokes. Needs the
# generated playback sources (.artifacts/playback/sources).
param(
  [string]$Shots = '.artifacts/timeline/ui',
  [string]$Exe,
  [int]$Width = 1800, [int]$Height = 1050)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\ui.ps1"
. "$PSScriptRoot\find.ps1"
if (-not ('Dpi3B' -as [type])) {
  Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Dpi3B { [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h); }
"@
}
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Exe) { $Exe = "$repo\apps\CutCap.Desktop\bin\Debug\net10.0\win-x64\CutCap.exe" }
New-Item -ItemType Directory -Force $Shots | Out-Null
$Shots = (Resolve-Path $Shots).Path
# Disposable copies: the missing-media step renames one of them.
$work = Join-Path $repo '.artifacts\timeline\work'
if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
New-Item -ItemType Directory -Force $work | Out-Null
$interview = Join-Path $work 'interview take.mp4'
$colors = Join-Path $work 'colors 视频.mkv'
Copy-Item "$repo\.artifacts\playback\sources\long-1080p.mp4" $interview
Copy-Item "$repo\tests\fixtures\colors-cfr.mkv" $colors
$project = Join-Path $work 'timeline edit.cutcap'

$log = [System.Collections.Generic.List[string]]::new()
function Note([string]$text) { $line = "{0:HH:mm:ss.fff} {1}" -f (Get-Date), $text; $log.Add($line); Write-Output $line }
function Lanes([IntPtr]$h) { (Find-Element $h 'Track lanes').Current.Name }
function Wait-Lanes([IntPtr]$h, [string]$fragment, [int]$ms = 8000) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  while ($w.ElapsedMilliseconds -lt $ms) { $n = Lanes $h; if ($n.Contains($fragment)) { return "$($w.ElapsedMilliseconds) ms" }; Start-Sleep -Milliseconds 60 }
  throw "lanes did not show '$fragment' within $ms ms (now '$(Lanes $h)')"
}
function Enabled([IntPtr]$h, [string]$name) { (Find-Element $h $name).Current.IsEnabled }
function Help([IntPtr]$h, [string]$name) { (Find-Element $h $name).Current.HelpText }
function State([IntPtr]$h, [string]$label) {
  Note "$label | lanes='$(Lanes $h)' undo=$(Enabled $h 'Undo') redo=$(Enabled $h 'Redo') delete=$(Enabled $h 'Delete clip') add=$(Enabled $h 'Add to timeline') title='$((Get-Process -Id $script:CutCapPid).MainWindowTitle)'"
}
# Clicks a point of the lanes surface given in logical timeline coordinates.
function Click-Lane([IntPtr]$h, [double]$ms, [int]$track, [int]$zoom = 100) {
  $scale = [Dpi3B]::GetDpiForWindow($h) / 96.0
  $r = (Find-Element $h 'Track lanes').Current.BoundingRectangle
  $x = 12 + $ms / 1000.0 * 20 * $zoom / 100; $y = $track * 50 + 25
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos([int]($r.X + $x * $scale), [int]($r.Y + $y * $scale)); Start-Sleep -Milliseconds 80
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40; [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 350
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
  # Never walk the dialog's descendants: its folder view makes that unbounded.
  # Alt+N focuses the File name box in both the Open and Save dialogs.
  Start-Sleep -Milliseconds 300
  Assert-CutCapForeground
  Add-Type -AssemblyName System.Windows.Forms
  $escaped = ($path -replace '([+^%~(){}\[\]])', '{$1}')
  [System.Windows.Forms.SendKeys]::SendWait('%n'); Start-Sleep -Milliseconds 200
  Assert-CutCapForeground
  [System.Windows.Forms.SendKeys]::SendWait("^a$escaped{ENTER}")
}
# Filters the library by name so the tile is on screen, selects it, then clears the filter.
function Select-Source([IntPtr]$h, [string]$fragment, [string]$name) {
  Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Type-Text $h $fragment; Start-Sleep -Milliseconds 500
  Click-Named $h $name; Start-Sleep -Milliseconds 700
  Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Send-Key $h 'BACK'; Start-Sleep -Milliseconds 500
  Note "selected source '$name': add tip='$(Help $h 'Add to timeline')' inspector='$(Read-Text $h 'Source video')'"
}
# A new untitled project is dirty by design, so Open first asks about it. The
# scenario's untitled project is empty: answer "Don't save", then pick the file.
function Open-Project([IntPtr]$h, [int]$ProcessId) {
  Send-Key $h 'CTRL+O'; Start-Sleep -Milliseconds 900
  Complete-Dialog $ProcessId 'Open CutCap project' $project
  # The app confirms replacing the dirty untitled project after the file is chosen.
  for ($i = 0; $i -lt 20; $i++) {
    $prompt = @(All-Elements $h | Where-Object { $_.Current.Name -eq 'Continue without saving' -and (Visible $_) })
    if ($prompt.Count) { Note 'unsaved prompt shown for the untitled project; continuing without saving'; Click-Named $h 'Continue without saving'; return }
    if ((Lanes $h) -notmatch ', 0 clips') { return }
    Start-Sleep -Milliseconds 250
  }
}function Import([IntPtr]$h, [int]$ProcessId, [string]$path) {
  Send-Key $h 'CTRL+I'; Start-Sleep -Milliseconds 700
  Complete-Dialog $ProcessId 'Import source video' $path; Start-Sleep -Milliseconds 2500
}
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
  Start-Sleep -Milliseconds 400
  $left = @(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count
  Note "$label | exited=$exited in $($w.ElapsedMilliseconds) ms code=$(if ($exited) { $app.P.ExitCode } else { 'n/a' }) leftover=$left"
}
$script:launch = 1

# ---- Insert, select, remove, undo, redo -------------------------------------
$app = Start-App; $h = $app.H
State $h 'launch'
Import $h $app.P.Id $interview
Import $h $app.P.Id $colors
Save-Client $h "$Shots\01-imported.jpg"
Note "add tip with source selected: '$(Help $h 'Add to timeline')'"
Select-Source $h 'interview' 'interview take.mp4'
Note "add tip: '$(Help $h 'Add to timeline')'"
Click-Named $h 'Add to timeline'; Note "first add: $(Wait-Lanes $h '1 clip')"
State $h 'added interview (20 s) to V1'
Save-Client $h "$Shots\02-first-clip.jpg"
Select-Source $h 'colors' 'colors 视频.mkv'
Click-Named $h 'Add to timeline'; Note "second add: $(Wait-Lanes $h '2 clips')"
State $h 'appended colors after interview'
Save-Client $h "$Shots\03-appended.jpg"
Click-Named $h 'Add video track'; Start-Sleep -Milliseconds 800
Select-Source $h 'interview' 'interview take.mp4'
Note "add tip with V2 selected: '$(Help $h 'Add to timeline')'"
Click-Named $h 'Add to timeline'; Note "third add: $(Wait-Lanes $h '3 clips')"
State $h 'added interview to the selected V2'
Save-Client $h "$Shots\04-second-track.jpg"

Click-Lane $h 5000 0
Note "clicked V1 at 5 s: $(Wait-Lanes $h 'Selected clip interview take.mp4')"
Note "inspector: '$(Read-Text $h 'Video clip on')'"
State $h 'selected first clip'
Save-Client $h "$Shots\05-selected.jpg"
Click-Lane $h 21000 0
Note "clicked V1 at 21 s: $(Wait-Lanes $h 'Selected clip colors')"
Click-Lane $h 5000 0; [void](Wait-Lanes $h 'Selected clip interview take.mp4')
Click-Named $h 'Media collections'  # keyboard focus outside text fields
Send-Key $h 'DEL'; Note "Delete: $(Wait-Lanes $h 'Track lanes, 2 clips')"
State $h 'deleted the selected clip'
Save-Client $h "$Shots\06-deleted.jpg"
Send-Key $h 'CTRL+Z'; Note "Ctrl+Z: $(Wait-Lanes $h 'Selected clip interview take.mp4')"
State $h 'undo restores the clip and its selection'
Save-Client $h "$Shots\07-undo.jpg"
Send-Key $h 'CTRL+Y'; Note "Ctrl+Y: $(Wait-Lanes $h 'Track lanes, 2 clips')"
Send-Key $h 'CTRL+SHIFT+Z'; Start-Sleep -Milliseconds 400
State $h 'Ctrl+Shift+Z with nothing to redo'
Send-Key $h 'CTRL+Z'; Note "Ctrl+Z again: $(Wait-Lanes $h '3 clips')"
Click-Named $h 'Undo'; Note "toolbar Undo: $(Wait-Lanes $h 'Track lanes, 2 clips')"
Click-Named $h 'Redo'; Note "toolbar Redo: $(Wait-Lanes $h '3 clips')"
State $h 'after toolbar undo/redo'

# Text fields keep their own Delete and Ctrl+Z.
Click-Named $h 'Search imported media by name'; Type-Text $h 'zz'; Send-Key $h 'CTRL+Z'; Send-Key $h 'DEL'
Start-Sleep -Milliseconds 500
Note "keys in the search field: search='$(Read-Text $h '' 'Search imported media by name')' lanes='$(Lanes $h)'"
Send-Key $h 'CTRL+A'; Send-Key $h 'BACK'; Start-Sleep -Milliseconds 300

# ---- Zoom, resize, playback independence ------------------------------------
Click-Named $h 'Zoom in'; Click-Named $h 'Zoom in'; Start-Sleep -Milliseconds 400
Note "zoom: '$(Read-Text $h '' 'Reset zoom')'"
Save-Client $h "$Shots\08-zoom-150.jpg"
Click-Lane $h 21000 0 150; Note "click at 21 s @150%: $(Wait-Lanes $h 'Selected clip colors')"
Click-Named $h 'Zoom out'; Click-Named $h 'Zoom out'; Click-Named $h 'Zoom out'; Start-Sleep -Milliseconds 400
Save-Client $h "$Shots\09-zoom-75.jpg"
Click-Named $h 'Reset zoom'
[void][Native2]::MoveWindow($h, 40, 20, 1100, 720, $true); Start-Sleep -Milliseconds 900
Save-Client $h "$Shots\10-small-window.jpg"
[void][Native2]::MoveWindow($h, 40, 20, $Width, $Height, $true); Start-Sleep -Milliseconds 700
Select-Source $h 'interview' 'interview take.mp4'
Click-Named $h 'Play'; Start-Sleep -Milliseconds 1500
Note "playing: button=$(@(All-Elements $h | Where-Object { $_.Current.Name -in 'Play','Pause' -and (Visible $_) } | ForEach-Object { $_.Current.Name }) -join ',') lanes='$(Lanes $h)'"
Save-Client $h "$Shots\11-playing-with-clips.jpg"
Click-Lane $h 21000 0; Note "select clip during playback: $(Wait-Lanes $h 'Selected clip colors')"
Click-Named $h 'Media collections'; Send-Key $h 'CTRL+Z'; Start-Sleep -Milliseconds 600
Note "undo during playback: button=$(@(All-Elements $h | Where-Object { $_.Current.Name -in 'Play','Pause' -and (Visible $_) } | ForEach-Object { $_.Current.Name }) -join ',') lanes='$(Lanes $h)'"
Send-Key $h 'CTRL+Y'; [void](Wait-Lanes $h '3 clips')
Click-Named $h 'Pause'; Start-Sleep -Milliseconds 400

# ---- Save, reopen ------------------------------------------------------------
Send-Key $h 'CTRL+SHIFT+S'; Start-Sleep -Milliseconds 900
Complete-Dialog $app.P.Id 'Save CutCap project' $project; Start-Sleep -Milliseconds 1800
State $h 'saved'
Save-Client $h "$Shots\12-saved.jpg"
Send-Key $h 'CTRL+N'; Start-Sleep -Milliseconds 1500
State $h 'new project'
Open-Project $h $app.P.Id
Note "reopen: $(Wait-Lanes $h '3 clips' 15000)"
Start-Sleep -Milliseconds 1500
State $h 'reopened'
Save-Client $h "$Shots\13-reopened.jpg"
Click-Lane $h 5000 2; Note "select reopened V2 clip: $(Wait-Lanes $h 'Selected clip interview take.mp4')"
Note "inspector: '$(Read-Text $h 'Video clip on')'"
Save-Client $h "$Shots\14-reopened-selected.jpg"
Close-App $app 'close after reopen (clean)'

# ---- Missing media -----------------------------------------------------------
Rename-Item -LiteralPath $interview 'interview take (moved).mp4'
$app = Start-App; $h = $app.H
Open-Project $h $app.P.Id
Note "reopen with a missing source: $(Wait-Lanes $h '3 clips' 15000)"
Start-Sleep -Milliseconds 2500
Click-Lane $h 5000 0; Note "select missing clip: $(Wait-Lanes $h 'source unavailable')"
State $h 'missing source'
Select-Source $h 'interview' 'interview take.mp4'
Note "add tip for missing source: '$(Help $h 'Add to timeline')' enabled=$(Enabled $h 'Add to timeline')"
Save-Client $h "$Shots\15-missing.jpg"
Close-App $app 'close with missing media (clean)'
Rename-Item -LiteralPath (Join-Path $work 'interview take (moved).mp4') 'interview take.mp4'
$log | Set-Content "$Shots\scenario.txt"
