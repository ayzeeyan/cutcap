# Phase 5A native export workflow. Dot-sourced by export-ui.ps1 through
# filmstrip-ui.ps1 -Perf -Stage (helpers and the foreground guard). Every edit goes
# through the real UI; each result is decoded independently by FFmpeg/FFprobe and
# compared with the positions actually saved in the project file.
. "$PSScriptRoot\av-common.ps1"
$sources = Join-Path $repo '.artifacts\export\sources'
foreach ($n in 'scene a.mp4', 'scene b.mp4', 'tone c.wav', 'tone d.wav') { Copy-Item (Join-Path $sources $n) (Join-Path $work $n) }
$project = Join-Path $work 'export session.cutcap'
$results = [ordered]@{}

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
# Owned windows appear to automation as children of the main window.
function Owned-Window([IntPtr]$h, [string]$like, [int]$timeoutMs = 5000) {
  $A = [System.Windows.Automation.AutomationElement]; $T = [System.Windows.Automation.TreeScope]
  $w = [Diagnostics.Stopwatch]::StartNew()
  while ($w.ElapsedMilliseconds -lt $timeoutMs) {
    $hit = @($A::FromHandle($h).FindAll($T::Children, [System.Windows.Automation.Condition]::TrueCondition) |
      Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Window' -and $_.Current.Name -like $like })
    if ($hit.Count) { return [IntPtr]$hit[0].Current.NativeWindowHandle }
    Start-Sleep -Milliseconds 80
  }
  throw "window '$like' did not open"
}
# The header list can still be settling after a drop: confirm the toggle took.
function Toggle-Until([IntPtr]$h, [string]$name, [string]$after) {
  Start-Sleep -Milliseconds 500
  for ($try = 1; $try -le 3; $try++) {
    Click-Named $h $name; Start-Sleep -Milliseconds 400
    if (@(All-Elements $h | Where-Object { $_.Current.Name -eq $after -and (Visible $_) }).Count) { return }
  }
  throw "'$name' did not take effect"
}
function Selected-Start-Ms([IntPtr]$h) {
  if ((Lanes $h) -match 'Selected clip .*?, (\d+):(\d+)\.(\d+) to') { return [int]$Matches[1] * 60000 + [int]$Matches[2] * 1000 + [int]$Matches[3] }
  throw "no selected clip: $(Lanes $h)"
}
function Export-Value([IntPtr]$x, [string]$name) { (Find-Element $x $name).Current.Name }
function Text-Of([IntPtr]$x, [string]$automation) {
  $e = @(All-Elements $x | Where-Object { $_.Current.AutomationId -eq $automation -or $_.Current.Name -eq $automation } | Select-Object -First 1)
  if ($e.Count) { $e[0].Current.Name } else { '' }
}
# The Avalonia text peer exposes its content as the Name, not the automation name.
function State-Text([IntPtr]$x) {
  $names = @(All-Elements $x | ForEach-Object { $_.Current.Name })
  @($names | Where-Object { $_ -in 'Checking the project and sources', 'Mixing audio', 'Starting the first decoder', 'Encoding', 'Finishing the file', 'Stopping the export', 'Export finished', 'Export cancelled', 'Export failed' } | Select-Object -First 1)
}
function Wait-State([IntPtr]$x, [string[]]$states, [int]$ms = 240000) {
  $w = [Diagnostics.Stopwatch]::StartNew()
  while ($w.ElapsedMilliseconds -lt $ms) {
    $s = State-Text $x
    if ($s -in $states) { return "$s" }
    if ($s -eq 'Export failed' -and $states -notcontains 'Export failed') { throw "export failed: $((All-Elements $x | ForEach-Object { $_.Current.Name } | Where-Object { $_ -match 'failed:' }) -join ' / ')" }
    Start-Sleep -Milliseconds 100
  }
  throw "export did not reach $($states -join '/') (now '$(State-Text $x)')"
}
function Choose-Output([IntPtr]$x, [string]$path, [bool]$replace) {
  Click-Named $x 'Choose output file'; Start-Sleep -Milliseconds 900
  Assert-CutCapForeground
  Add-Type -AssemblyName System.Windows.Forms
  [System.Windows.Forms.SendKeys]::SendWait('%n'); Start-Sleep -Milliseconds 200; Assert-CutCapForeground
  [System.Windows.Forms.SendKeys]::SendWait("^a$($path -replace '([+^%~(){}\[\]])', '{$1}'){ENTER}")
  Start-Sleep -Milliseconds 700
  if ($replace) {
    # Windows asks "Do you want to replace it?" in its own confirmation.
    Assert-CutCapForeground
    [System.Windows.Forms.SendKeys]::SendWait('%y'); Start-Sleep -Milliseconds 500
  }
}
function Open-Export($h) { Click-Named $h 'Export'; Start-Sleep -Milliseconds 700; Owned-Window $h 'Export — CutCap' }
function Run-Export([IntPtr]$x, [string]$path, [string]$shot = '') {
  Click-Named $x 'Start export'
  $w = [Diagnostics.Stopwatch]::StartNew()
  if ($shot) {
    $null = Wait-State $x @('Encoding', 'Export finished') 60000
    Start-Sleep -Milliseconds 400
    Save-Client $x $shot
  }
  $final = Wait-State $x @('Export finished')
  Note "   export finished in $($w.ElapsedMilliseconds) ms: $final; temporary files left: $(@(Get-ChildItem -LiteralPath (Split-Path $path) -Force -Filter '*.cutcap-*').Count)"
}
function Probe([string]$path) { & ffprobe -v error -show_entries 'stream=codec_type,codec_name,width,height,pix_fmt,nb_frames,avg_frame_rate,sample_rate,channels,color_space,color_primaries,color_transfer,color_range:format=format_name,duration' -of json $path | Out-String | ConvertFrom-Json }
function Rgb([string]$path, [double]$t) {
  $file = Join-Path $work 'rgb.raw'
  & ffmpeg -hide_banner -v error -y -ss ([string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:0.000}', $t)) -i $path -frames:v 1 -f rawvideo -pix_fmt rgb24 $file
  $b = [IO.File]::ReadAllBytes($file)
  $at = (360 * 1280 + 640) * 3
  @([int]$b[$at], [int]$b[$at + 1], [int]$b[$at + 2])
}
function Named-Color($c) {
  $names = [ordered]@{ red = @(255, 0, 0); lime = @(0, 255, 0); blue = @(0, 0, 255); white = @(255, 255, 255); yellow = @(255, 255, 0); cyan = @(0, 255, 255); magenta = @(255, 0, 255); black = @(0, 0, 0) }
  foreach ($k in $names.Keys) { if ([Math]::Abs($c[0] - $names[$k][0]) -lt 40 -and [Math]::Abs($c[1] - $names[$k][1]) -lt 40 -and [Math]::Abs($c[2] - $names[$k][2]) -lt 40) { return $k } }
  "rgb($($c -join ','))"
}
function Pcm([string]$path) {
  $file = Join-Path $work 'pcm.raw'
  & ffmpeg -hide_banner -v error -y -i $path -map 0:a:0 -ac 1 -ar 48000 -f f32le -c:a pcm_f32le $file
  $b = [IO.File]::ReadAllBytes($file)
  $s = New-Object 'single[]' ($b.Length / 4)
  [Buffer]::BlockCopy($b, 0, $s, 0, $b.Length)
  , $s
}
function Level($pcm, [double]$a, [double]$b, [double]$hz) {
  $from = [int]($a * 48000); $to = [Math]::Min([int]($b * 48000), $pcm.Length)
  $w = 2 * [Math]::PI * $hz / 48000; $re = 0.0; $im = 0.0
  for ($i = $from; $i -lt $to; $i++) { $p = $w * ($i - $from); $re += $pcm[$i] * [Math]::Cos($p); $im -= $pcm[$i] * [Math]::Sin($p) }
  [Math]::Round(2 * [Math]::Sqrt($re * $re + $im * $im) / ($to - $from), 4)
}
function Expect([bool]$condition, [string]$what) {
  if (-not $condition) { throw "VERIFY FAILED: $what" }
  Note "   ok: $what"
}

$script:launch = 1
$app = Start-App; $h = $app.H
Note "scale=$(Scale $h)"
Save-Client $h "$Shots\00-export-disabled-without-clips.jpg"

# 1-2. Import both videos; place A by drag.
Import $h $app.P.Id (Join-Path $work 'scene a.mp4')
$from = Tile $h 'scene a' 'scene a.mp4'
Drag-Points $from (Lane-Point $h 300 0) 30
Clear-Search $h
Note "1 video A placed: $(Wait-Lanes $h '2 clips')"
Import $h $app.P.Id (Join-Path $work 'scene b.mp4')
Import $h $app.P.Id (Join-Path $work 'tone c.wav')
Import $h $app.P.Id (Join-Path $work 'tone d.wav')

# 4. Trim A's start by two seconds: the red scene goes.
$startA = Selected-Start-Ms $h
$edge = Lane-Point $h $startA 0
$edge.X += [int](3 * (Scale $h))
Drag-Points $edge (Lane-Point $h ($startA + 2000) 0) 20
Note "4 trimmed: lanes='$(Lanes $h)'"
# 6. Split B later; 5. place B first. Timeline: A then a gap then B.
$from = Tile $h 'scene b' 'scene b.mp4'
Drag-Points $from (Lane-Point $h 11000 0) 30
Clear-Search $h
Note "5 video B placed: $(Wait-Lanes $h '4 clips')"
Click-Point (Ruler-Point $h 13000); Start-Sleep -Milliseconds 900
Click-Point (Lane-Point $h 12500 0); Click-Named $h 'Media collections'
Send-Key $h 'CTRL+B'
Note "6 split B: $(Wait-Lanes $h '6 clips')"
# 7. Move B's second part later: another gap.
Drag-Points (Lane-Point $h 15000 0) (Lane-Point $h 17000 0) 24
Start-Sleep -Milliseconds 500
Note "7 moved: lanes='$(Lanes $h)'"
# 8. Independent audio on a new track.
Click-Named $h 'Add audio track'; Start-Sleep -Milliseconds 500
$from = Tile $h 'tone c' 'tone c.wav'
Drag-Points $from (Lane-Point $h 1300 2) 30
Clear-Search $h
Note "8 audio-only placed: $(Wait-Lanes $h '7 clips')"
# 9. Audio 2 down 6 dB.
$null = Drag-Gain $h 'Audio 2' 0 -6
Note "9 gain: $(Gain-Text $h 'Audio 2')"
# 10. A muted track, which must not be heard.
Click-Named $h 'Add audio track'; Start-Sleep -Milliseconds 500
$from = Tile $h 'tone d' 'tone d.wav'
Drag-Points $from (Lane-Point $h 300 3) 30
Clear-Search $h
Note "10 tone placed: $(Wait-Lanes $h '8 clips')"
Toggle-Until $h 'Mute Audio 3' 'Unmute Audio 3'
Click-Named $h 'Media collections'; Start-Sleep -Milliseconds 2500
Save-Client $h "$Shots\01-edited-timeline.jpg"

# 11. Save, and read what the timeline really is.
Send-Key $h 'CTRL+SHIFT+S'; Start-Sleep -Milliseconds 900
Complete-Dialog $app.P.Id 'Save CutCap project' $project; Start-Sleep -Milliseconds 2000
$doc = Get-Content -LiteralPath $project -Raw -Encoding utf8 | ConvertFrom-Json
$tb = $doc.project.timebase
$sec = { param($t) [double]$t * $tb.numerator / $tb.denominator }
$tracks = @($doc.project.tracks)
$media = @{}; foreach ($m in $doc.project.media) { $media[$m.id] = Split-Path $m.path -Leaf }
$video = @($doc.project.clips | Where-Object { $_.stream.kind -eq 'video' } | Sort-Object { [long]$_.start })
Note "11 saved schema $($doc.project.schema_version); video clips: $(@($video | ForEach-Object { "$($media[$_.media_id]) $(& $sec $_.start)..$((& $sec $_.start) + (& $sec $_.duration)) in $($_.source_in.ticks)/$($_.source_in.timebase.denominator)" }) -join ' ; ')"
$results.video_clips = @($video | ForEach-Object { [ordered]@{ media = $media[$_.media_id]; start = & $sec $_.start; end = (& $sec $_.start) + (& $sec $_.duration); source_in = $_.source_in.ticks * $_.source_in.timebase.numerator / $_.source_in.timebase.denominator } })
Expect ($video.Count -eq 3) 'three video clips after trim, split and move'

# 12-15. Export with the dialog.
$x = Open-Export $h
Save-Client $x "$Shots\02-export-dialog.jpg"
$out1 = Join-Path $work 'finished edit.mp4'
Choose-Output $x $out1 $false
Save-Client $x "$Shots\03-dialog-ready.jpg"
Run-Export $x $out1 "$Shots\04-exporting.jpg"
Start-Sleep -Milliseconds 300
Save-Client $x "$Shots\05-finished.jpg"
Expect (Test-Path -LiteralPath $out1) 'the file exists'

# 16-20. Independent checks of the first export.
$p = Probe $out1
$v = @($p.streams | Where-Object codec_type -eq 'video')[0]
$a = @($p.streams | Where-Object codec_type -eq 'audio')[0]
$duration = [double]::Parse($p.format.duration, [Globalization.CultureInfo]::InvariantCulture)
$frames = [int]$v.nb_frames
Expect ($v.codec_name -eq 'h264' -and $v.width -eq 1280 -and $v.height -eq 720 -and $v.pix_fmt -eq 'yuv420p' -and $v.avg_frame_rate -eq '30/1') 'H.264, 1280x720, yuv420p, 30 fps'
Expect ($v.color_space -eq 'bt709' -and $v.color_primaries -eq 'bt709' -and $v.color_transfer -eq 'bt709' -and $v.color_range -eq 'tv') 'BT.709 limited range tags'
Expect ($a.codec_name -eq 'aac' -and $a.sample_rate -eq '48000' -and $a.channels -eq 2) 'AAC, 48 kHz, stereo'
Note "   ffprobe: $frames frames, $duration s"
$results.first = [ordered]@{ frames = $frames; duration = $duration; bytes = (Get-Item -LiteralPath $out1).Length }
$colors = @{ 'scene a.mp4' = @('red', 'lime', 'blue', 'white'); 'scene b.mp4' = @('yellow', 'cyan', 'magenta') }
$checks = @()
foreach ($clip in $video) {
  $s = & $sec $clip.start; $e = $s + (& $sec $clip.duration)
  $in = [double]$clip.source_in.ticks * $clip.source_in.timebase.numerator / $clip.source_in.timebase.denominator
  foreach ($t in @(($s + 0.15), (($s + $e) / 2), ($e - 0.15))) {
    if ($t -ge $frames / 30) { continue }
    $scene = [int][Math]::Floor(($t - $s + $in) / 2)
    $wanted = $colors[$media[$clip.media_id]][$scene]
    $found = Named-Color (Rgb $out1 $t)
    $checks += "$([Math]::Round($t, 2))=$found"
    Expect ($found -eq $wanted) "at $([Math]::Round($t, 2)) s the picture is $wanted (clip from $($media[$clip.media_id]))"
  }
}
for ($i = 0; $i -lt $video.Count - 1; $i++) {
  $gapFrom = (& $sec $video[$i].start) + (& $sec $video[$i].duration); $gapTo = & $sec $video[$i + 1].start
  if ($gapTo - $gapFrom -gt 0.5) {
    $mid = ($gapFrom + $gapTo) / 2
    Expect ((Named-Color (Rgb $out1 $mid)) -eq 'black') "the gap at $([Math]::Round($mid, 2)) s is black"
  }
}
$pcm = Pcm $out1
$a1 = $tracks | Where-Object name -eq 'Audio 1'
$a2 = $tracks | Where-Object name -eq 'Audio 2'
$a3 = $tracks | Where-Object name -eq 'Audio 3'
$levels = [ordered]@{}
foreach ($c in @($doc.project.clips | Where-Object { $_.track_id -eq $a1.id } | Sort-Object { [long]$_.start })) {
  $s = & $sec $c.start; $e = $s + (& $sec $c.duration)
  $hz = if ($media[$c.media_id] -eq 'scene a.mp4') { 400 } else { 800 }
  $level = Level $pcm ($s + 0.1) ($e - 0.1) $hz
  $levels["$hz Hz $s..$e"] = $level
  Expect ($level -gt 0.05) "$hz Hz is present in $s..$e ($level)"
}
$c2 = @($doc.project.clips | Where-Object { $_.track_id -eq $a2.id })[0]
$s2 = & $sec $c2.start; $e2 = $s2 + (& $sec $c2.duration)
$level = Level $pcm ($s2 + 0.1) ($e2 - 0.1) 1200
Expect ($level -gt 0.03) "the independent audio clip is present ($level)"
$levels['1200 Hz'] = $level
$off = Level $pcm 0.1 ($s2 - 0.1) 1200
Expect ($off -lt 0.01) "and absent before it starts ($off)"
$muted = Level $pcm 0.1 ($frames / 30 - 0.1) 1600
Expect ($muted -lt 0.005) "the muted track is not heard ($muted)"
$results.first.levels = $levels
$firstHash = (Get-FileHash -LiteralPath $out1).Hash

# 21. The Windows media stack that the inbox player uses, without showing a window.
$mf = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$PSScriptRoot\export-player-check.ps1" -Path $out1 2>&1 | Out-String
Note "21 Windows Media Foundation: $($mf.Trim())"
$results.windows_player = $mf.Trim()

# 22-23. Change the project and export again: the new snapshot is what is written.
Click-Named $x 'Close export window'; Start-Sleep -Milliseconds 400
Toggle-Until $h 'Unmute Audio 3' 'Mute Audio 3'
$x = Open-Export $h
$out2 = Join-Path $work 'second export.mp4'
Choose-Output $x $out2 $false
Run-Export $x $out2
$pcm2 = Pcm $out2
$level = Level $pcm2 0.1 ($frames / 30 - 0.1) 1600
Expect ($level -gt 0.05) "unmuting reaches the new export ($level)"
Expect ((Get-FileHash -LiteralPath $out1).Hash -eq $firstHash) 'the first export is untouched'

# 24-25. Start another export over the first file and cancel it.
Choose-Output $x $out1 $true
Click-Named $x 'Start export'
$null = Wait-State $x @('Encoding') 60000
Start-Sleep -Milliseconds 600
$cancelAt = [Diagnostics.Stopwatch]::StartNew()
Click-Named $x 'Cancel export'
$null = Wait-State $x @('Export cancelled') 30000
$results.cancel_ms = $cancelAt.ElapsedMilliseconds
Note "24 cancelled in $($cancelAt.ElapsedMilliseconds) ms"
Save-Client $x "$Shots\06-cancelled.jpg"
Start-Sleep -Milliseconds 300
Expect ((Get-FileHash -LiteralPath $out1).Hash -eq $firstHash) 'the earlier completed file is preserved'
Expect (@(Get-ChildItem -LiteralPath $work -Force -Filter '*.cutcap-*').Count -eq 0) 'no temporary files remain'
Expect (@(Get-Process ffmpeg -ErrorAction SilentlyContinue).Count -eq 0) 'no FFmpeg process remains after cancelling'

# 26-27. Close during an export. A long source keeps it running through the prompt.
Click-Named $x 'Close export window'; Start-Sleep -Milliseconds 400
$longName = 'av long 1080p.mp4'
Copy-Item (Join-Path $repo ".artifacts\av\sources\$longName") (Join-Path $work $longName)
Import $h $app.P.Id (Join-Path $work $longName)
Start-Sleep -Milliseconds 800
Click-Named $h 'Add to timeline'; Start-Sleep -Milliseconds 300
Note "26 long source added: $(Wait-Lanes $h '10 clips' 15000)"
$x = Open-Export $h
Choose-Output $x (Join-Path $work 'never finished.mp4') $false
Click-Named $x 'Start export'
$null = Wait-State $x @('Encoding') 60000
Start-Sleep -Milliseconds 500
$running = @(Get-Process ffmpeg -ErrorAction SilentlyContinue).Count
[void]$app.P.CloseMainWindow(); Start-Sleep -Milliseconds 900
Save-Client $h "$Shots\07-close-prompt.jpg"
$prompt = Owned-Window $h 'Export in progress*'
Note '   ok: closing asks before stopping the export'
$closing = [Diagnostics.Stopwatch]::StartNew()
Click-Named $prompt 'Stop export and close'; Start-Sleep -Milliseconds 200
for ($i = 0; $i -lt 40 -and -not $app.P.HasExited; $i++) {
  if (@(All-Elements $app.H | Where-Object { $_.Current.Name -eq 'Continue without saving' -and (Visible $_) }).Count) { Click-Named $app.H 'Continue without saving' }
  Start-Sleep -Milliseconds 100
}
$exited = $app.P.WaitForExit(15000)
Start-Sleep -Milliseconds 400
$results.close_during_export = [ordered]@{ ffmpeg_before = $running; exited = $exited; ms = $closing.ElapsedMilliseconds; leftover = @(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count }
Note "26 closed during export with $running FFmpeg running: exited=$exited in $($closing.ElapsedMilliseconds) ms, leftover=$($results.close_during_export.leftover)"
Expect ($exited -and $results.close_during_export.leftover -eq 0) 'no process remains after closing'
Expect (-not (Test-Path -LiteralPath (Join-Path $work 'never finished.mp4'))) 'no partial file was published'
Expect (@(Get-ChildItem -LiteralPath $work -Force -Filter '*.cutcap-*').Count -eq 0) 'no temporary files remain after closing'

# Older projects: each opens and exports without being rewritten.
$legacy = Join-Path $repo '.artifacts\av\legacy'
$one = Join-Path $work 'schema1 project.cutcap'
Copy-Item (Join-Path $repo '.artifacts\phase3d\native-two-source.cutcap') $one
Copy-Item (Join-Path $repo '.artifacts\phase3d\native-media') (Join-Path $work 'native-media') -Recurse
$two = Join-Path $work 'schema2 project.cutcap'
Copy-Item (Join-Path $legacy 'schema2.cutcap') $two
Copy-Item (Join-Path $legacy 'av moved.mp4') (Join-Path $work 'av moved.mp4')
foreach ($old in @([ordered]@{ Schema = 1; File = $one }, [ordered]@{ Schema = 2; File = $two }, [ordered]@{ Schema = 3; File = $project })) {
  $project = $old.File
  $hashBefore = (Get-FileHash -LiteralPath $old.File).Hash
  $app = Start-App; $h = $app.H
  Open-Project $h $app.P.Id
  Start-Sleep -Milliseconds 2500
  $lanes = Lanes $h
  $x = Open-Export $h
  $out = Join-Path $work "from schema $($old.Schema).mp4"
  Choose-Output $x $out $false
  # A project with several occupied video tracks asks first.
  if (@(All-Elements $x | Where-Object { $_.Current.Name -eq 'Export only the chosen track' -and (Visible $_) }).Count) { Click-Named $x 'Export only the chosen track'; Start-Sleep -Milliseconds 200 }
  Run-Export $x $out
  $p = Probe $out
  $f = @($p.streams | Where-Object codec_type -eq 'video')[0]
  Note "27 schema $($old.Schema): lanes='$lanes'; exported $($f.nb_frames) frames; project file unchanged: $((Get-FileHash -LiteralPath $old.File).Hash -eq $hashBefore)"
  Expect ((Get-FileHash -LiteralPath $old.File).Hash -eq $hashBefore) "exporting did not rewrite the schema $($old.Schema) project"
  Expect ([int]$f.nb_frames -gt 0 -and $f.codec_name -eq 'h264') "the schema $($old.Schema) project exported"
  $results["schema$($old.Schema)"] = [ordered]@{ frames = [int]$f.nb_frames; lanes = $lanes }
  Click-Named $x 'Close export window'; Start-Sleep -Milliseconds 300
  $null = Close-Discard $app "close schema $($old.Schema)"
}

$results.leftover_processes = @(Get-Process CutCap, ffmpeg, ffprobe -ErrorAction SilentlyContinue).Count
$results | ConvertTo-Json -Depth 8 | Set-Content "$Shots\export-results.json" -Encoding utf8
$log | Set-Content "$Shots\export-scenario.txt" -Encoding utf8
