# Phase 2B native playback UI scenario over real input. Sends input only while
# CutCap owns the foreground (ui.ps1 aborts otherwise). Needs the generated
# playback sources (.artifacts/playback/sources, see scripts/generate-playback-fixtures.ps1).
param(
  [string]$Shots = '.artifacts/playback/ui',
  [string]$Exe,
  [int]$Width = 1800, [int]$Height = 1050)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\ui.ps1"
. "$PSScriptRoot\find.ps1"
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Exe) { $Exe = "$repo\apps\CutCap.Desktop\bin\Debug\net10.0\win-x64\CutCap.exe" }
$src = "$repo\.artifacts\playback\sources"
$fx = "$repo\tests\fixtures"
New-Item -ItemType Directory -Force $Shots | Out-Null
$log = [System.Collections.Generic.List[string]]::new()
function Note([string]$text) { $line = "{0:HH:mm:ss.fff} {1}" -f (Get-Date), $text; $log.Add($line); Write-Output $line }
function PlayName([IntPtr]$h) {
  $names = @(All-Elements $h | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' -and $_.Current.Name -in 'Play', 'Pause' -and (Visible $_) } | ForEach-Object { $_.Current.Name })
  if ($names.Count) { $names[0] } else { '?' }
}
function Chip([IntPtr]$h) {
  $c = @(All-Elements $h | Where-Object { $_.Current.Name -in 'Starting…', 'Seeking…', 'Buffering…', 'End of source', 'Playback failed' -and (Visible $_) } | ForEach-Object { $_.Current.Name })
  if ($c.Count) { $c[0] } else { '-' }
}
function State([IntPtr]$h, [string]$label) { Note "$label | button=$(PlayName $h) chip=$(Chip $h) goto=$(Read-Text $h '' 'Go to source time')" }
function Wait-PlayName([IntPtr]$h, [string]$name, [int]$ms = 8000) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  while ($w.ElapsedMilliseconds -lt $ms) { if ((PlayName $h) -eq $name) { return $w.ElapsedMilliseconds }; Start-Sleep -Milliseconds 50 }
  throw "button did not become '$name' within $ms ms (chip $(Chip $h))"
}
function Wait-Chip([IntPtr]$h, [string]$chip, [int]$ms = 12000) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  while ($w.ElapsedMilliseconds -lt $ms) { if ((Chip $h) -eq $chip) { return $w.ElapsedMilliseconds }; Start-Sleep -Milliseconds 50 }
  throw "chip did not become '$chip' within $ms ms"
}
function GoTo([IntPtr]$h, [string]$text) { Click-Named $h 'Go to source time'; Send-Key $h 'CTRL+A'; Type-Text $h $text; Send-Key $h 'ENTER' }
function Start-App {
  $p = Start-Process $Exe -WorkingDirectory $repo -PassThru -RedirectStandardError "$Shots\app-err-$($script:launch).log" -RedirectStandardOutput "$Shots\app-out-$($script:launch).log"
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

$app = Start-App; $h = $app.H
foreach ($f in "$src\long-1080p.mp4", "$src\720p.mp4", "$src\4k.mp4", "$fx\colors-vfr.mkv") { Import-Fixture $h $app.P.Id $f }
Start-Sleep -Milliseconds 1200
Click-Named $h 'long-1080p.mp4'; Start-Sleep -Milliseconds 1500
State $h 'selected 20 s 1080p'
Save-Client $h "$Shots\01-selected.jpg"

# Play / readout advances / Pause holds / Space resumes.
Click-Named $h 'Play'; Note "Play -> Pause after $(Wait-PlayName $h 'Pause') ms"
Start-Sleep -Milliseconds 900; Save-Client $h "$Shots\02-playing-a.jpg"
Start-Sleep -Milliseconds 700; Save-Client $h "$Shots\03-playing-b.jpg"
Click-Named $h 'Pause'; Note "Pause -> Play after $(Wait-PlayName $h 'Play') ms"
Start-Sleep -Milliseconds 300; Save-Client $h "$Shots\04-paused-a.jpg"
Start-Sleep -Milliseconds 800; Save-Client $h "$Shots\05-paused-b.jpg"
Click-Named $h 'All media'  # focus a list item (not a text field or button), then Space
Send-Key $h 'SPACE'; Note "Space -> $(PlayName $h) after $(Wait-PlayName $h 'Pause') ms"
Send-Key $h 'SPACE'; Note "Space -> $(PlayName $h) after $(Wait-PlayName $h 'Play') ms"
Click-Named $h 'Search imported media by name'; Send-Key $h 'SPACE'; Start-Sleep -Milliseconds 300
Note "Space in search field: button=$(PlayName $h) search='$(Read-Text $h '' 'Search imported media by name')'"
Send-Key $h 'BACK'

# Seek while paused, while playing, while stopped.
GoTo $h '1500'; Start-Sleep -Milliseconds 1500; State $h 'paused seek 1500'; Save-Client $h "$Shots\06-paused-seek.jpg"
Click-Named $h 'Play'; [void](Wait-PlayName $h 'Pause'); Start-Sleep -Milliseconds 500
GoTo $h '1000'; State $h 'playing seek 1000 (immediately)'
Start-Sleep -Milliseconds 1200; State $h 'playing seek 1000 (+1.2 s)'; Save-Client $h "$Shots\07-playing-seek.jpg"

# Rapid scrubbing while playing: click along the slider.
$r = (Find-Element $h 'Source position').Current.BoundingRectangle
foreach ($f in 0.2, 0.8, 0.4, 0.6, 0.3, 0.7) {
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos([int]($r.X + $r.Width * $f), [int]($r.Y + $r.Height / 2))
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 30; [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 90
}
Start-Sleep -Milliseconds 1500; State $h 'after 6 rapid scrubs'; Save-Client $h "$Shots\08-rapid-scrub.jpg"

# Stop: back to the start; then play to the end and replay.
Click-Named $h 'Stop'; Start-Sleep -Milliseconds 1200; State $h 'stopped'; Save-Client $h "$Shots\09-stopped.jpg"
GoTo $h '18500'; Start-Sleep -Milliseconds 1200; State $h 'stopped seek 18500'
Click-Named $h 'Play'; Note "play from 18500: End of source after $(Wait-Chip $h 'End of source') ms"
State $h 'ended'; Save-Client $h "$Shots\10-ended.jpg"
Click-Named $h 'Play'; Note "replay -> Pause after $(Wait-PlayName $h 'Pause') ms"; Start-Sleep -Milliseconds 800; State $h 'replaying'

# Window resize during playback.
[void][Native2]::MoveWindow($h, 40, 20, 1440, 900, $true); Start-Sleep -Milliseconds 800; Save-Client $h "$Shots\11-resized.jpg"
[void][Native2]::MoveWindow($h, 40, 20, $Width, $Height, $true); Start-Sleep -Milliseconds 600; State $h 'after resize'

# Switch source while playing.
Click-Named $h '720p.mp4'; Start-Sleep -Milliseconds 1500; State $h 'switched to 720p while playing'; Save-Client $h "$Shots\12-switched.jpg"
Click-Named $h 'Play'; [void](Wait-PlayName $h 'Pause'); Start-Sleep -Milliseconds 1000; Save-Client $h "$Shots\13-720p-playing.jpg"

# 4K, and Pause before its first frame arrives.
Click-Named $h '4k.mp4'; Start-Sleep -Milliseconds 2000
Click-Named $h 'Play'; Start-Sleep -Milliseconds 60; Click-Named $h 'Pause'
State $h '4K play then immediate pause'
Start-Sleep -Milliseconds 2000; State $h '4K +2 s after early pause'
Click-Named $h 'Play'; [void](Wait-PlayName $h 'Pause'); Start-Sleep -Milliseconds 1500; Save-Client $h "$Shots\14-4k-playing.jpg"

# VFR source to the end.
Click-Named $h 'colors-vfr.mkv'; Start-Sleep -Milliseconds 1500
Click-Named $h 'Play'; Note "VFR End of source after $(Wait-Chip $h 'End of source') ms"; Save-Client $h "$Shots\15-vfr-ended.jpg"

# New Project during playback, then a corrupt file.
Click-Named $h 'long-1080p.mp4'; Start-Sleep -Milliseconds 1500; Click-Named $h 'Play'; [void](Wait-PlayName $h 'Pause')
Send-Key $h 'CTRL+N'; Start-Sleep -Milliseconds 1500
Note "new project during playback: tiles=$(@(All-Elements $h | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.ListItem' -and $_.Current.Name -like '*.mp4,*' }).Count) button=$(PlayName $h) play-enabled=$((Find-Element $h 'Play').Current.IsEnabled)"
Save-Client $h "$Shots\16-new-project.jpg"
Import-Fixture $h $app.P.Id "$fx\corrupt.mp4"; Start-Sleep -Milliseconds 800; Save-Client $h "$Shots\17-corrupt.jpg"
Note "corrupt: status=$(Read-Text $h 'FFprobe')"

# Close during playback.
Import-Fixture $h $app.P.Id "$src\long-1080p.mp4"; Start-Sleep -Milliseconds 1500
Click-Named $h 'Play'; [void](Wait-PlayName $h 'Pause'); Start-Sleep -Milliseconds 800
Close-App $app 'close during 1080p playback'

# Close during a pending seek (4K seeks take ~0.5-1 s).
$app = Start-App; $h = $app.H
Import-Fixture $h $app.P.Id "$src\4k.mp4"; Start-Sleep -Milliseconds 2500
Click-Named $h 'Play'; [void](Wait-PlayName $h 'Pause'); Start-Sleep -Milliseconds 500
GoTo $h '4500'; Note "pending seek chip=$(Chip $h)"
Close-App $app 'close during pending 4K seek'
$log | Set-Content "$Shots\scenario.log"
