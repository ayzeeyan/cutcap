[CmdletBinding()]
param([ValidateRange(1,10)][int]$Runs = 3)
$ErrorActionPreference = 'Stop'
$rootPath = Split-Path -Parent $PSScriptRoot
$binaryPath = Join-Path $rootPath 'target\release\cutcap.exe'
$evidencePath = Join-Path $rootPath '.artifacts\performance'
$sourcePath = Join-Path $rootPath '.artifacts\performance-sources'
if (-not (Test-Path -LiteralPath $binaryPath)) { throw 'Build release first: cargo build --release' }
foreach ($name in @('720p.mp4','1080p.mp4','4k.mp4')) {
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePath $name))) { throw 'Generate inputs: ./scripts/generate-fixtures.ps1 -Performance' }
}
New-Item -ItemType Directory -Force -Path $evidencePath | Out-Null
$memoryResults = @()
for ($runIndex = 1; $runIndex -le $Runs; $runIndex++) {
    $reportPath = Join-Path $evidencePath "release-$runIndex.json"
    # Start-Process flattens its argument array; quote each literal Windows path.
    # These are workspace paths, not shell commands; no shell is launched.
    $arguments = @('--benchmark-preview',('"' + $sourcePath + '"'),('"' + $reportPath + '"'))
    $process = Start-Process -FilePath $binaryPath -ArgumentList $arguments -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $evidencePath "release-$runIndex-stdout.log") -RedirectStandardError (Join-Path $evidencePath "release-$runIndex-stderr.log")
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $peakWorkingSet = 0L
    $peakPrivate = 0L
    $samples = 0
    while (-not $process.HasExited) {
        $process.Refresh()
        if (-not $process.HasExited) {
            $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.WorkingSet64)
            $peakPrivate = [Math]::Max($peakPrivate, $process.PrivateMemorySize64)
            $samples++
        }
        if ($watch.Elapsed.TotalSeconds -gt 180) {
            # Scoped to this developer benchmark only; prevent a wedged validation run.
            # A forced parent termination cannot run Rust Drop; reap owned media
            # children first instead of leaving a decoder behind on this path.
            Get-CimInstance Win32_Process -Filter "ParentProcessId = $($process.Id)" | ForEach-Object {
                Stop-Process -Id $_.ProcessId -ErrorAction SilentlyContinue
            }
            Stop-Process -Id $process.Id
            throw 'Preview benchmark exceeded 180 seconds. Inspect logs and child processes.'
        }
        Start-Sleep -Milliseconds 100
    }
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Preview benchmark failed ($($process.ExitCode)): $reportPath" }
    $report = Get-Content -Raw -LiteralPath $reportPath | ConvertFrom-Json
    if (-not $report.completed -or $report.samples.Count -ne 30) { throw "Incomplete preview report: $reportPath" }
    $memoryResults += [ordered]@{ run=$runIndex; samples=$samples; intervalMs=100; appPeakWorkingSetBytes=$peakWorkingSet; appPeakPrivateBytes=$peakPrivate; seconds=$watch.Elapsed.TotalSeconds; note='Sampled application only; excludes FFmpeg/FFprobe child memory, native dialog and OS cache. Not a hard peak.' }
    Write-Host "Run $runIndex completed: 30 real frame presentations."
}
$memoryResults | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $evidencePath 'memory.json') -Encoding utf8
Write-Host "Reports: $evidencePath"
