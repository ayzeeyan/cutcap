# Phase 4A filmstrip performance stage. Dot-sourced by `filmstrip-ui.ps1 -Perf`
# (it reuses that script's helpers and foreground guard); not runnable alone.
# Launches CutCap with CUTCAP_TRACE_THUMBNAILS=1 and CUTCAP_TRACE_TIMELINE=1 on
# a generated 3-minute 1080p H.264 source (GOP 250) and marks each phase in the
# step log; filmstrip-perf-report.ps1 joins the marks with the app's trace.
$long = Join-Path $work 'long 1080p.mp4'
Copy-Item "$repo\.artifacts\filmstrip\sources\long 1080p.mp4" $long
function Shift-Wheel([int]$ticks, [int]$delta) {
  $wp = Wheel-Point $h
  Assert-CutCapForeground
  [void][Native2]::SetCursorPos($wp.X, $wp.Y); Start-Sleep -Milliseconds 80
  foreach ($i in 1..$ticks) {
    Assert-CutCapForeground
    [Native2]::keybd_event(0x10, 0, 0, [IntPtr]::Zero); [Native2]::mouse_event(0x0800, 0, 0, $delta, [IntPtr]::Zero); [Native2]::keybd_event(0x10, 0, 2, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 30
  }
}

$env:CUTCAP_TRACE_THUMBNAILS = '1'; $env:CUTCAP_TRACE_TIMELINE = '1'
$script:launch = 1
$app = Start-App; $h = $app.H
$env:CUTCAP_TRACE_THUMBNAILS = $null; $env:CUTCAP_TRACE_TIMELINE = $null
Note "scale=$(Scale $h) window=${Width}x${Height}"
Import $h $app.P.Id $long
Select-Source $h 'long' 'long 1080p.mp4'
Note 'MARK place'
Click-Named $h 'Add to timeline'; Note "placed: $(Wait-Lanes $h '1 clip')"
Start-Sleep -Milliseconds 8000
Click-Named $h 'Media collections'
Save-Client $h "$Shots\perf-01-placed-1080p.jpg"

Note 'MARK zoom-in'
Click-Named $h 'Zoom in'; Click-Named $h 'Zoom in'; Click-Named $h 'Zoom in'; Click-Named $h 'Zoom in'
Start-Sleep -Milliseconds 8000
Save-Client $h "$Shots\perf-02-zoom-200.jpg"
Note 'MARK zoom-reset'
Click-Named $h 'Reset zoom'; Start-Sleep -Milliseconds 4000

Note 'MARK scroll-away'
Shift-Wheel 30 -120; Start-Sleep -Milliseconds 8000
Note 'MARK scroll-back'
Shift-Wheel 45 120; Start-Sleep -Milliseconds 3000
Save-Client $h "$Shots\perf-03-scrolled-back.jpg"

Note 'MARK zoom-out'
Click-Named $h 'Zoom out'; Click-Named $h 'Zoom out'; Start-Sleep -Milliseconds 6000
Click-Named $h 'Reset zoom'; Start-Sleep -Milliseconds 2000

# Playback while thumbnails decode (new zoom grid), then with everything cached.
Click-Point (Ruler-Point $h 5000); Start-Sleep -Milliseconds 1200
Note 'MARK play-decoding'
Click-Named $h 'Zoom in'; Click-Named $h 'Play timeline'; Start-Sleep -Milliseconds 6000
Click-Named $h 'Pause timeline'; Note 'MARK pause-decoding'; Start-Sleep -Milliseconds 6000
Click-Point (Ruler-Point $h 5000); Start-Sleep -Milliseconds 1200
Note 'MARK play-cached'
Click-Named $h 'Play timeline'; Start-Sleep -Milliseconds 6000
Click-Named $h 'Pause timeline'; Note 'MARK pause-cached'; Start-Sleep -Milliseconds 800
Click-Named $h 'Reset zoom'; Start-Sleep -Milliseconds 3000

Send-Key $h 'CTRL+SHIFT+S'; Start-Sleep -Milliseconds 900
Complete-Dialog $app.P.Id 'Save CutCap project' $project; Start-Sleep -Milliseconds 2000
Note "saved title='$((Get-Process -Id $script:CutCapPid).MainWindowTitle)'"
Note 'MARK close-decoding'
Click-Named $h 'Zoom out'; Start-Sleep -Milliseconds 120
Close-App $app 'close during thumbnail decoding'
$log | Set-Content "$Shots\perf-scenario.txt" -Encoding utf8
