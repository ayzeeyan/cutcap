# Phase 4A native filmstrip scenario over real input (CutCap must own the
# foreground; ui.ps1 aborts otherwise). Uses a generated three-scene source
# (.artifacts/filmstrip/sources/scenes 30s.mp4: testsrc2 | SMPTE bars |
# Mandelbrot, 10 s each) and verifies drawn thumbnail pixels against frames
# decoded independently by FFmpeg at the same sample times.
param(
  [string]$Shots = '.artifacts/filmstrip/ui',
  [string]$Exe,
  [switch]$BeforeOnly,
  [switch]$Perf,
  [string]$Stage = 'filmstrip-perf.ps1',
  [int]$Width = 1800, [int]$Height = 1050)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\ui.ps1"
. "$PSScriptRoot\find.ps1"
if (-not ('Dpi4A' -as [type])) {
  Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Dpi4A { [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h); }
"@
}
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Exe) { $Exe = "$repo\apps\CutCap.Desktop\bin\Debug\net10.0\win-x64\CutCap.exe" }
New-Item -ItemType Directory -Force $Shots | Out-Null
$Shots = (Resolve-Path $Shots).Path
$work = Join-Path $repo '.artifacts\filmstrip\work'
if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
New-Item -ItemType Directory -Force $work | Out-Null
$scenes = Join-Path $work 'scenes 30s.mp4'
$colors = Join-Path $work 'colors.mkv'
Copy-Item "$repo\.artifacts\filmstrip\sources\scenes 30s.mp4" $scenes
Copy-Item "$repo\tests\fixtures\colors-cfr.mkv" $colors
$project = Join-Path $work 'filmstrip.cutcap'
$TrackHeight = if ($BeforeOnly) { 50 } else { 56 }

$log = [System.Collections.Generic.List[string]]::new()
function Note([string]$text) { $line = "{0:HH:mm:ss.fff} {1}" -f (Get-Date), $text; $log.Add($line); Write-Host $line }
function Lanes([IntPtr]$h) { (Find-Element $h 'Track lanes').Current.Name }
function Wait-Lanes([IntPtr]$h, [string]$fragment, [int]$ms = 8000) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  while ($w.ElapsedMilliseconds -lt $ms) { if ((Lanes $h).Contains($fragment)) { return "$($w.ElapsedMilliseconds) ms" }; Start-Sleep -Milliseconds 60 }
  throw "lanes did not show '$fragment' within $ms ms (now '$(Lanes $h)')"
}
function Scale([IntPtr]$h) { [Dpi4A]::GetDpiForWindow($h) / 96.0 }
function Lane-Point([IntPtr]$h, [double]$ms, [int]$track, [double]$dy = 0, [int]$zoom = 100) {
  $s = Scale $h; $r = (Find-Element $h 'Track lanes').Current.BoundingRectangle
  @{ X = [int]($r.X + (12 + $ms / 1000.0 * 20 * $zoom / 100) * $s); Y = [int]($r.Y + ($track * $TrackHeight + $TrackHeight / 2 + $dy) * $s) }
}
function Ruler-Point([IntPtr]$h, [double]$ms) {
  $s = Scale $h; $r = (Find-Element $h 'Timeline ruler').Current.BoundingRectangle
  @{ X = [int]($r.X + (12 + $ms / 1000.0 * 20) * $s); Y = [int]($r.Y + $r.Height / 2) }
}
function Click-Point($p) {
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos($p.X, $p.Y); Start-Sleep -Milliseconds 80
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40; [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 350
}
function Drag-Points($from, $to, [int]$steps = 24) {
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos($from.X, $from.Y); Start-Sleep -Milliseconds 120
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 120
  for ($i = 1; $i -le $steps; $i++) {
    Assert-CutCapForeground
    [void][Native2]::SetCursorPos([int]($from.X + ($to.X - $from.X) * $i / $steps), [int]($from.Y + ($to.Y - $from.Y) * $i / $steps)); Start-Sleep -Milliseconds 25
  }
  Start-Sleep -Milliseconds 250
  [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 700
}
function Complete-Dialog([int]$ProcessId, [string]$title, [string]$path) {
  $A = [System.Windows.Automation.AutomationElement]; $T = [System.Windows.Automation.TreeScope]
  $main = $A::FromHandle((Get-CutCapWindow $ProcessId)); $dlg = $null
  for ($i = 0; $i -lt 50 -and -not $dlg; $i++) {
    $dlg = $main.FindAll($T::Children, [System.Windows.Automation.Condition]::TrueCondition) | Where-Object { $_.Current.Name -eq $title } | Select-Object -First 1
    if (-not $dlg) { Start-Sleep -Milliseconds 150 }
  }
  if (-not $dlg) { throw "ABORT: dialog '$title' not found" }
  Start-Sleep -Milliseconds 300; Assert-CutCapForeground
  Add-Type -AssemblyName System.Windows.Forms
  [System.Windows.Forms.SendKeys]::SendWait('%n'); Start-Sleep -Milliseconds 200; Assert-CutCapForeground
  [System.Windows.Forms.SendKeys]::SendWait("^a$($path -replace '([+^%~(){}\[\]])', '{$1}'){ENTER}")
}
function Import([IntPtr]$h, [int]$ProcessId, [string]$path) {
  Send-Key $h 'CTRL+I'; Start-Sleep -Milliseconds 700
  Complete-Dialog $ProcessId 'Import source video' $path; Start-Sleep -Milliseconds 2500
}
function Open-Project([IntPtr]$h, [int]$ProcessId) {
  Send-Key $h 'CTRL+O'; Start-Sleep -Milliseconds 900
  Complete-Dialog $ProcessId 'Open CutCap project' $project
  $w = [Diagnostics.Stopwatch]::StartNew()
  while ($w.ElapsedMilliseconds -lt 30000) {
    if (@(All-Elements $h | Where-Object { $_.Current.Name -eq 'Continue without saving' -and (Visible $_) }).Count) { Click-Named $h 'Continue without saving' }
    if ((Lanes $h) -notmatch ', 0 clips') { return }
    Start-Sleep -Milliseconds 250
  }
}
function Select-Source([IntPtr]$h, [string]$fragment, [string]$name) {
  Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Type-Text $h $fragment; Start-Sleep -Milliseconds 500
  Click-Named $h $name; Start-Sleep -Milliseconds 700
  Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Send-Key $h 'BACK'; Start-Sleep -Milliseconds 500
}
function Start-App {
  $p = Start-Process $Exe -WorkingDirectory $repo -PassThru -RedirectStandardError "$Shots\app-err-$($script:launch).txt" -RedirectStandardOutput "$Shots\app-out-$($script:launch).txt"
  $script:launch++; $script:CutCapPid = $p.Id
  $h = Get-CutCapWindow $p.Id; Start-Sleep -Milliseconds 1500
  [void][Native2]::MoveWindow($h, 40, 20, $Width, $Height, $true); Start-Sleep -Milliseconds 700
  @{ P = $p; H = $h }
}
function Close-App($app, [string]$label) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  [void]$app.P.CloseMainWindow(); $exited = $app.P.WaitForExit(10000); Start-Sleep -Milliseconds 500
  $left = @(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count
  Note "$label | exited=$exited in $($w.ElapsedMilliseconds) ms code=$(if ($exited) { $app.P.ExitCode } else { 'n/a' }) leftover=$left"
}
Add-Type -AssemblyName System.Drawing
# Mean RGB of a physical-pixel rectangle of the screen (CutCap is foreground).
function Mean-Screen([int]$x, [int]$y, [int]$w, [int]$h) {
  Assert-CutCapForeground
  $bmp = New-Object System.Drawing.Bitmap $w, $h; $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size $w, $h)); $g.Dispose()
  $r = 0; $gg = 0; $b = 0; $n = 0
  for ($i = 0; $i -lt $w; $i += 2) { for ($j = 0; $j -lt $h; $j += 2) { $c = $bmp.GetPixel($i, $j); $r += $c.R; $gg += $c.G; $b += $c.B; $n++ } }
  $bmp.Dispose(); @([int]($r / $n), [int]($gg / $n), [int]($b / $n))
}
# The same frame decoded independently (ceiling PTS policy, scaled like the cell), mean RGB of the same sub-rectangle.
function Mean-Frame([string]$file, [double]$seconds, [int]$w, [int]$h, [double]$fx, [double]$fy, [double]$fw, [double]$fh) {
  $raw = Join-Path $work 'oracle.raw'
  $ticks = [int64][Math]::Ceiling($seconds * 15360)
  & ffmpeg -hide_banner -v error -y -copyts -i $file -vf "select=gte(pts\,$ticks),scale=${w}:${h}" -frames:v 1 -f rawvideo -pix_fmt rgb24 $raw 2>&1 | Out-Null
  $bytes = [IO.File]::ReadAllBytes($raw); $r = 0; $g = 0; $b = 0; $n = 0
  for ($x = [int]($fx * $w); $x -lt [int](($fx + $fw) * $w); $x += 2) { for ($y = [int]($fy * $h); $y -lt [int](($fy + $fh) * $h); $y += 2) { $o = ($y * $w + $x) * 3; $r += $bytes[$o]; $g += $bytes[$o + 1]; $b += $bytes[$o + 2]; $n++ } }
  @([int]($r / $n), [int]($g / $n), [int]($b / $n))
}
function Distance($a, $b) { [Math]::Sqrt([Math]::Pow($a[0] - $b[0], 2) + [Math]::Pow($a[1] - $b[1], 2) + [Math]::Pow($a[2] - $b[2], 2)) }
# Verify one filmstrip cell of a clip whose source in-point is $inUs, placed at $clipMs, on $track at 100% zoom.
$StripHeight = $TrackHeight - 2 * 5 - 3
$CellWidth = $StripHeight * 16 / 9
$StepUs = [Math]::Round($CellWidth / 20 * 1e6)
function Check-Cell([IntPtr]$h, [double]$clipMs, [int]$track, [double]$inUs, [int]$k, [string]$label) {
  $s = Scale $h; $r = (Find-Element $h 'Track lanes').Current.BoundingRectangle
  $sampleUs = $k * $StepUs
  $cellX = 12 + $clipMs / 1000.0 * 20 + ($sampleUs - $inUs) / 1e6 * 20
  # Lower 60% of the cell (below the name band), inset 4 DIP.
  $x = [int]($r.X + ($cellX + 4) * $s); $y = [int]($r.Y + ($track * $TrackHeight + 5 + 1.5 + $StripHeight * 0.4) * $s)
  $w = [int](($CellWidth - 8) * $s); $hh = [int]($StripHeight * 0.55 * $s)
  $screen = Mean-Screen $x $y $w $hh
  $oracle = Mean-Frame $scenes ($sampleUs / 1e6) 320 180 (4 / $CellWidth) 0.4 (($CellWidth - 8) / $CellWidth) 0.55
  $d = [Math]::Round((Distance $screen $oracle), 1)
  Note "$label cell k=$k sample=$([Math]::Round($sampleUs / 1e6, 3)) s screen=$($screen -join ',') ffmpeg=$($oracle -join ',') distance=$d match=$($d -lt 40)"
  $d
}
$script:launch = 1
if ($Perf) { . $(if ([IO.Path]::IsPathRooted($Stage)) { $Stage } else { Join-Path $PSScriptRoot $Stage }); return }

$app = Start-App; $h = $app.H
Import $h $app.P.Id $scenes
Import $h $app.P.Id $colors
Select-Source $h 'scenes' 'scenes 30s.mp4'
Click-Named $h 'Add to timeline'; Note "placed: $(Wait-Lanes $h '1 clip')"
Start-Sleep -Milliseconds 4000
Click-Named $h 'Media collections'
Save-Client $h "$Shots\$(if ($BeforeOnly) { '00-before-phase3e' } else { '01-filmstrip' }).jpg"
if ($BeforeOnly) {
  [void][Native2]::MoveWindow($h, 40, 20, 1440, 900, $true); Start-Sleep -Milliseconds 2500
  Save-Client $h "$Shots\00-before-phase3e-minimum.jpg"
  # The placed clip is unsaved: answer the prompt so the old build exits.
  [void]$app.P.CloseMainWindow(); Start-Sleep -Milliseconds 1200
  if (@(All-Elements $h | Where-Object { $_.Current.Name -eq 'Continue without saving' }).Count) { Click-Named $h 'Continue without saving' }
  Note "before (Phase 3E build) exited=$($app.P.WaitForExit(10000))"
  $log | Set-Content "$Shots\before.txt" -Encoding utf8; return
}

# 3-4. Real frames that differ by scene: scene 1 (0-10 s), 2 (10-20 s), 3 (20-30 s).
$d = @()
foreach ($k in 0, 3, 6) { $d += Check-Cell $h 0 0 0 $k 'placed' }
Note "distinct scenes: s1-s2=$([Math]::Round((Distance (Mean-Frame $scenes 0 64 36 0 0 1 1) (Mean-Frame $scenes ($StepUs*3/1e6) 64 36 0 0 1 1)),1))"

# 5-6. Zoom in (denser detail) and out (fewer cells).
Click-Named $h 'Zoom in'; Click-Named $h 'Zoom in'; Click-Named $h 'Zoom in'; Click-Named $h 'Zoom in'; Start-Sleep -Milliseconds 3500
Save-Client $h "$Shots\02-zoom-200.jpg"
Click-Named $h 'Reset zoom'; Click-Named $h 'Zoom out'; Click-Named $h 'Zoom out'; Start-Sleep -Milliseconds 3000
Save-Client $h "$Shots\03-zoom-50.jpg"
Click-Named $h 'Reset zoom'; Start-Sleep -Milliseconds 1500

# 7-8. Scroll away and back (cached thumbnails reused: no placeholders on return).
# The wheel goes over the empty audio lane, where no clip tooltip can take it.
$lanes = (Find-Element $h 'Track lanes').Current.BoundingRectangle
Assert-CutCapForeground
[void][Native2]::SetCursorPos([int]($lanes.X + 600), [int]($lanes.Y + 1.5 * $TrackHeight * (Scale $h)))
foreach ($i in 1..25) { Assert-CutCapForeground; [Native2]::keybd_event(0x10, 0, 0, [IntPtr]::Zero); [Native2]::mouse_event(0x0800, 0, 0, -120, [IntPtr]::Zero); [Native2]::keybd_event(0x10, 0, 2, [IntPtr]::Zero); Start-Sleep -Milliseconds 30 }
Start-Sleep -Milliseconds 800
Save-Client $h "$Shots\04-scrolled-away.jpg"
# Proof that the view moved: the k=0 position no longer shows the k=0 frame.
$away = Check-Cell $h 0 0 0 0 'scrolled away (expected mismatch)'
if ($away -lt 40) { throw "the lanes did not scroll away (k=0 still matches at $away)" }
foreach ($i in 1..40) { Assert-CutCapForeground; [Native2]::keybd_event(0x10, 0, 0, [IntPtr]::Zero); [Native2]::mouse_event(0x0800, 0, 0, 120, [IntPtr]::Zero); [Native2]::keybd_event(0x10, 0, 2, [IntPtr]::Zero); Start-Sleep -Milliseconds 30 }
Start-Sleep -Milliseconds 300
Save-Client $h "$Shots\04-scrolled-back.jpg"
foreach ($k in 0, 3) { $null = Check-Cell $h 0 0 0 $k 'after scroll back' }

# 9-10. Trim the start by 5 s: the strip now starts at source 5 s (scene 1, later part).
$p0 = Lane-Point $h 0 0; $p0.X += [int](3 * (Scale $h))
Drag-Points $p0 (Lane-Point $h 5000 0) 20
Note "trimmed: $(Wait-Lanes $h '00:05.000 to 00:30.000')"
Start-Sleep -Milliseconds 3000
Save-Client $h "$Shots\05-trimmed.jpg"
$k0 = [Math]::Ceiling(5e6 / $StepUs)
$null = Check-Cell $h 5000 0 5e6 $k0 'trimmed (first grid cell after 5 s)'

# 11-12. Split at 15 s: left = source 5-15 s, right = 15-30 s.
Click-Point (Ruler-Point $h 15000); Start-Sleep -Milliseconds 1000
Click-Point (Lane-Point $h 8000 0); Click-Named $h 'Media collections'
Send-Key $h 'CTRL+B'; Note "split: $(Wait-Lanes $h '2 clips')"
Start-Sleep -Milliseconds 2500
Save-Client $h "$Shots\06-split.jpg"
$kRight = [Math]::Ceiling(15e6 / $StepUs)
$null = Check-Cell $h 5000 0 5e6 $k0 'split left'
$null = Check-Cell $h 15000 0 15e6 $kRight 'split right'

# 13-14. Move the right part to Video 2... (only one video track): move it later on Video 1.
Drag-Points (Lane-Point $h 22000 0) (Lane-Point $h 30000 0)
Note "moved: $(Wait-Lanes $h '00:23.000 to 00:38.000')"
Start-Sleep -Milliseconds 2500
$null = Check-Cell $h 23000 0 15e6 $kRight 'moved right part'
Save-Client $h "$Shots\07-moved.jpg"

# 15. Undo/redo the move.
Send-Key $h 'CTRL+Z'; Note "undo move: $(Wait-Lanes $h '00:15.000 to 00:30.000')"
Send-Key $h 'CTRL+Y'; Note "redo move: $(Wait-Lanes $h '00:23.000 to 00:38.000')"
Start-Sleep -Milliseconds 1500

# 20. Timeline playback still works with filmstrips drawn.
Click-Point (Ruler-Point $h 6000); Start-Sleep -Milliseconds 1200
Click-Named $h 'Play timeline'; Start-Sleep -Milliseconds 2500
Note "timeline playing: $(@(All-Elements $h | Where-Object { $_.Current.Name -like '*timeline' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' } | ForEach-Object { $_.Current.Name }) -join ',')"
Save-Client $h "$Shots\08-playing.jpg"
Click-Named $h 'Pause timeline'; Start-Sleep -Milliseconds 400

# 16-17. Save, close, reopen: thumbnails regenerate, the document stays clean.
Send-Key $h 'CTRL+SHIFT+S'; Start-Sleep -Milliseconds 900
Complete-Dialog $app.P.Id 'Save CutCap project' $project; Start-Sleep -Milliseconds 2000
Note "saved title='$((Get-Process -Id $script:CutCapPid).MainWindowTitle)'"
[void][Native2]::MoveWindow($h, 40, 20, 1440, 900, $true); Start-Sleep -Milliseconds 2500
Save-Client $h "$Shots\09-minimum-window.jpg"
Close-App $app 'close after save'

$app = Start-App; $h = $app.H
Open-Project $h $app.P.Id
Note "reopened: $(Wait-Lanes $h '2 clips' 15000)"
Start-Sleep -Milliseconds 5000
Note "reopened title='$((Get-Process -Id $script:CutCapPid).MainWindowTitle)' (clean: no '*')"
$null = Check-Cell $h 5000 0 5e6 $k0 'reopened left'
Save-Client $h "$Shots\10-reopened.jpg"
Close-App $app 'close after reopen'

# 18-19. Missing media: no images; relink recovers them.
Rename-Item -LiteralPath $scenes 'scenes moved.mp4'
$app = Start-App; $h = $app.H
Open-Project $h $app.P.Id
Note "reopened with missing source: $(Wait-Lanes $h '2 clips' 15000)"
Start-Sleep -Milliseconds 3500
Save-Client $h "$Shots\11-missing.jpg"
Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Type-Text $h 'scenes'; Start-Sleep -Milliseconds 500
Click-Named $h 'scenes 30s.mp4'; Start-Sleep -Milliseconds 500
Click-Named $h 'Project menu'; Start-Sleep -Milliseconds 500
Click-Named $h 'Relink selected source…'; Start-Sleep -Milliseconds 900
Complete-Dialog $app.P.Id 'Relink original source video' (Join-Path $work 'scenes moved.mp4')
Start-Sleep -Milliseconds 6000
Click-Named $h 'Search imported media by name'; Send-Key $h 'CTRL+A'; Send-Key $h 'BACK'; Start-Sleep -Milliseconds 2500
Save-Client $h "$Shots\12-relinked.jpg"
$null = Check-Cell $h 5000 0 5e6 $k0 'relinked left'

# Relinking is a document edit: save it so the close below measures shutdown only, not the unsaved prompt.
Send-Key $h 'CTRL+S'; Start-Sleep -Milliseconds 1500
Note "relink saved title='$((Get-Process -Id $script:CutCapPid).MainWindowTitle)'"

# 21-22. Close while thumbnails decode: zoom (new grid, new samples) then close at once.
Click-Named $h 'Zoom in'; Click-Named $h 'Zoom in'; Start-Sleep -Milliseconds 150
Close-App $app 'close during thumbnail decoding'
$log | Set-Content "$Shots\scenario.txt" -Encoding utf8
