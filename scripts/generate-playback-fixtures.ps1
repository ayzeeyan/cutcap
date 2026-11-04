[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$workspacePath = Split-Path -Parent $PSScriptRoot
$sourcePath = Join-Path $workspacePath '.artifacts/playback/sources'
New-Item -ItemType Directory -Force -Path $sourcePath | Out-Null
& ffmpeg -hide_banner -loglevel error -nostdin -y -f lavfi -i 'testsrc2=size=320x180:rate=30000/1001:duration=4' -c:v libx264 -threads 2 -preset veryfast -crf 20 -g 120 -keyint_min 120 -bf 3 -sc_threshold 0 -pix_fmt yuv420p (Join-Path $workspacePath 'tests/fixtures/long-gop-bframes.mp4')
if ($LASTEXITCODE -ne 0) { throw 'Long-GOP regression fixture generation failed.' }
# Five-second synthetic H.264 sources with a single long GOP and B-frames.
# Generated media only; no executable, footage or external font is copied.
foreach ($source in @(@{Name='720p';Size='1280x720'},@{Name='1080p';Size='1920x1080'},@{Name='4k';Size='3840x2160'})) {
    & ffmpeg -hide_banner -loglevel error -nostdin -y -f lavfi -i "testsrc2=size=$($source.Size):rate=30:duration=5" -c:v libx264 -threads 2 -preset veryfast -crf 20 -g 150 -keyint_min 150 -bf 3 -sc_threshold 0 -pix_fmt yuv420p (Join-Path $sourcePath "$($source.Name).mp4")
    if ($LASTEXITCODE -ne 0) { throw "Playback fixture generation failed: $($source.Name)" }
}
Write-Output "Playback sources: $sourcePath"
