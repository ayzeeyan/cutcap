# Phase 5A native export workflow (CutCap must own the foreground while input is
# sent). Build first: cargo build -p cutcap-ffi, then dotnet build CutCap.slnx.
# Generates its own sources under .artifacts/export/sources. No user media is used.
param([string]$Shots = '.artifacts/export/ui', [string]$Exe, [int]$Width = 1800, [int]$Height = 1050)
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$sources = Join-Path $root '.artifacts\export\sources'
New-Item -ItemType Directory -Force $sources | Out-Null
function Scene([string]$name, [string[]]$colors, [double]$step, [int]$hz, [int]$fps) {
  $path = Join-Path $sources $name
  if (Test-Path -LiteralPath $path) { return }
  $total = $colors.Count * $step
  $filter = "color=c=black:s=1280x720:r=${fps}:d=$total"
  for ($i = 0; $i -lt $colors.Count; $i++) {
    $filter += ",drawbox=c=$($colors[$i]):t=fill:enable='gte(t,$($i * $step))*lt(t,$(($i + 1) * $step))'"
  }
  & ffmpeg -hide_banner -v error -y -f lavfi -i $filter -f lavfi -i "sine=frequency=${hz}:sample_rate=48000:duration=$total,volume=2" `
    -map 0:v -map 1:a -c:v libx264 -g 60 -bf 2 -crf 18 -pix_fmt yuv420p -c:a aac -b:a 256k $path
}
function Wav([string]$name, [int]$hz, [double]$seconds) {
  $path = Join-Path $sources $name
  if (Test-Path -LiteralPath $path) { return }
  & ffmpeg -hide_banner -v error -y -f lavfi -i "sine=frequency=${hz}:sample_rate=48000:duration=$seconds,volume=2" -c:a pcm_s16le $path
}
Scene 'scene a.mp4' @('red', 'lime', 'blue', 'white') 2 400 30
Scene 'scene b.mp4' @('yellow', 'cyan', 'magenta') 2 800 24
Wav 'tone c.wav' 1200 4
Wav 'tone d.wav' 1600 18
$splat = @{ Shots = $Shots; Perf = $true; Stage = 'export-scenario.ps1'; Width = $Width; Height = $Height }
if ($Exe) { $splat.Exe = $Exe }
$started = Get-Date
try { & "$PSScriptRoot\filmstrip-ui.ps1" @splat } finally {
  # A failed run must not leave its app behind and skew the next one.
  Get-Process CutCap -ErrorAction SilentlyContinue | Where-Object { $_.StartTime -ge $started } | Stop-Process -Force
}
