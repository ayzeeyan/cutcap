# Phase 4C native multi-track workflow (CutCap must own the foreground; plays
# test tones through the default output). Build first:
#   cargo build -p cutcap-ffi --features audio-diagnostics --bin cutcap-audio-loopback
#   dotnet build apps/CutCap.Desktop/CutCap.Desktop.csproj
# Generates its sources under .artifacts/av/sources-mix. No user media is used.
param([string]$Shots = '.artifacts/av/mix-ui', [string]$Exe, [int]$Width = 1800, [int]$Height = 1050)
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$sources = Join-Path $root '.artifacts\av\sources-mix'
New-Item -ItemType Directory -Force $sources | Out-Null
$film = Join-Path $sources 'mix film.mp4'
if (-not (Test-Path -LiteralPath $film)) {
  & ffmpeg -hide_banner -v error -y -f lavfi -i 'testsrc2=size=1280x720:rate=30:duration=30' `
    -f lavfi -i 'sine=frequency=400:sample_rate=48000:duration=30,volume=2' `
    -f lavfi -i 'sine=frequency=1200:sample_rate=48000:duration=30,volume=2' `
    -map 0:v -map 1:a -map 2:a -c:v libx264 -preset veryfast -g 30 -pix_fmt yuv420p -c:a aac -b:a 192k `
    -disposition:a:0 default -disposition:a:1 0 $film
}
$music = Join-Path $sources 'mix music.wav'
if (-not (Test-Path -LiteralPath $music)) {
  & ffmpeg -hide_banner -v error -y -f lavfi -i 'sine=frequency=800:sample_rate=48000:duration=30,volume=2' -c:a pcm_s16le -ac 2 $music
}
$splat = @{ Shots = $Shots; Perf = $true; Stage = 'mix-scenario.ps1'; Width = $Width; Height = $Height }
if ($Exe) { $splat.Exe = $Exe }
& "$PSScriptRoot\filmstrip-ui.ps1" @splat
