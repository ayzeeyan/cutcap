# Phase 4C native mixer measurements (CutCap must own the foreground; plays
# quiet tones through the default output). Build first, as for mix-ui.ps1. The
# 180 s sync source comes from the Phase 4B bench.
param([string]$Shots = '.artifacts/av/mix-bench', [string]$Exe, [int]$LongSeconds = 60, [int]$Width = 1800, [int]$Height = 1050)
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$sources = Join-Path $root '.artifacts\av\sources-mix'
New-Item -ItemType Directory -Force $sources | Out-Null
$tone = Join-Path $sources 'mix bench tone.m4a'
if (-not (Test-Path -LiteralPath $tone)) {
  # Well below full scale, so sixteen in-phase copies cannot clip.
  & ffmpeg -hide_banner -v error -y -f lavfi -i 'sine=frequency=700:sample_rate=48000:duration=120,volume=0.4' -c:a aac -b:a 128k $tone
}
$splat = @{ Shots = $Shots; Perf = $true; Stage = 'mix-bench.ps1'; Width = $Width; Height = $Height }
if ($Exe) { $splat.Exe = $Exe }
$global:LongSeconds = $LongSeconds
$started = Get-Date
try { & "$PSScriptRoot\filmstrip-ui.ps1" @splat } finally {
  Remove-Variable LongSeconds -Scope Global -ErrorAction SilentlyContinue
  # A failed run must not leave its app behind and skew the next measurement.
  Get-Process CutCap -ErrorAction SilentlyContinue | Where-Object { $_.StartTime -ge $started } | Stop-Process -Force
}
