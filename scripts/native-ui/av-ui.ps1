# Phase 4B native audio/video workflow (CutCap must own the foreground).
# Builds nothing: run `cargo build -p cutcap-ffi --features audio-diagnostics
# --bin cutcap-audio-loopback` and the Debug desktop app first. The default
# Windows output device must be active; its volume affects only loudness, not
# the loopback timing.
param([string]$Shots = '.artifacts/av/ui', [string]$Exe, [int]$Width = 1800, [int]$Height = 1050)
$splat = @{ Shots = $Shots; Perf = $true; Stage = 'av-scenario.ps1'; Width = $Width; Height = $Height }
if ($Exe) { $splat.Exe = $Exe }
& "$PSScriptRoot\filmstrip-ui.ps1" @splat
