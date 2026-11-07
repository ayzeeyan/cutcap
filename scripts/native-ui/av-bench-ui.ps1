# Phase 4B native A/V measurements (CutCap must own the foreground; plays
# audible clicks through the default output). Build first:
#   cargo build -p cutcap-ffi --features audio-diagnostics --bin cutcap-audio-loopback
#   dotnet build apps/CutCap.Desktop/CutCap.Desktop.csproj
# Set CUTCAP_AUDIO_LATENCY_MS to measure with output latency compensation.
param([string]$Shots = '.artifacts/av/bench', [string]$Exe, [int]$LongSeconds = 60, [int]$Width = 1800, [int]$Height = 1050)
$splat = @{ Shots = $Shots; Perf = $true; Stage = 'av-bench.ps1'; Width = $Width; Height = $Height }
if ($Exe) { $splat.Exe = $Exe }
$global:LongSeconds = $LongSeconds
try { & "$PSScriptRoot\filmstrip-ui.ps1" @splat } finally { Remove-Variable LongSeconds -Scope Global -ErrorAction SilentlyContinue }
