# Opens a file through Windows Media Foundation (the stack the inbox Media Player
# uses) with no window, and reports what it found. Must run under Windows
# PowerShell 5.1, which can project the WinRT media types.
param([Parameter(Mandatory)][string]$Path)
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Media.Playback.MediaPlayer, Windows.Media.Playback, ContentType = WindowsRuntime]
$null = [Windows.Media.Core.MediaSource, Windows.Media.Core, ContentType = WindowsRuntime]
$player = New-Object Windows.Media.Playback.MediaPlayer
$player.IsMuted = $true
$player.Source = [Windows.Media.Core.MediaSource]::CreateFromUri([Uri]::new($Path))
$session = $player.PlaybackSession
$watch = [Diagnostics.Stopwatch]::StartNew()
while ($watch.ElapsedMilliseconds -lt 15000 -and $session.NaturalDuration.Ticks -eq 0) { Start-Sleep -Milliseconds 100 }
if ($session.NaturalDuration.Ticks -eq 0) { Write-Output 'NOT OPENED'; exit 1 }
$player.Play()
Start-Sleep -Milliseconds 1200
$moved = $session.Position.TotalMilliseconds
$state = $session.PlaybackState
$player.Pause()
Write-Output ("opened, {0:0.000} s, {1}x{2}, position after 1.2 s of play {3:0} ms, state {4}" -f $session.NaturalDuration.TotalSeconds, $session.NaturalVideoWidth, $session.NaturalVideoHeight, $moved, $state)
$player.Dispose()
