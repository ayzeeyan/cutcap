[CmdletBinding()]
param(
    # Skip the self-contained win-x64 publish (the slowest stage).
    [switch]$SkipPublish
)

$ErrorActionPreference = 'Stop'
$workspacePath = Split-Path -Parent $PSScriptRoot
$evidencePath = Join-Path $workspacePath '.artifacts\verification'
New-Item -ItemType Directory -Path $evidencePath -Force | Out-Null
# Rust first: the .NET builds copy the cutcap_engine.dll these stages produce.
$checks = @(
    @{ Name = 'fmt'; Tool = 'cargo'; Args = @('fmt', '--all', '--', '--check') },
    @{ Name = 'clippy'; Tool = 'cargo'; Args = @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings') },
    @{ Name = 'test'; Tool = 'cargo'; Args = @('test', '--workspace') },
    @{ Name = 'build'; Tool = 'cargo'; Args = @('build', '--workspace') },
    @{ Name = 'release'; Tool = 'cargo'; Args = @('build', '--release') },
    @{ Name = 'dotnet-build'; Tool = 'dotnet'; Args = @('build', 'CutCap.slnx', '-c', 'Debug', '-nologo') },
    @{ Name = 'dotnet-test'; Tool = 'dotnet'; Args = @('test', 'CutCap.slnx', '-c', 'Debug', '-nologo', '--no-build') },
    @{ Name = 'dotnet-release'; Tool = 'dotnet'; Args = @('build', 'CutCap.slnx', '-c', 'Release', '-nologo') }
)
if (-not $SkipPublish) {
    $checks += @{ Name = 'publish'; Tool = 'dotnet'; Args = @('publish', 'apps/CutCap.Desktop/CutCap.Desktop.csproj', '-c', 'Release', '-r', 'win-x64', '--self-contained', 'true', '-o', 'artifacts/publish/win-x64', '-nologo') }
}
$results = @()
Push-Location -LiteralPath $workspacePath
try {
    foreach ($check in $checks) {
        Write-Host "Running $($check.Tool) $($check.Args -join ' ')"
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $arguments = $check.Args
        & $check.Tool @arguments 2>&1 | Tee-Object -FilePath (Join-Path $evidencePath "$($check.Name).log") | ForEach-Object { Write-Host $_ }
        $resultCode = $LASTEXITCODE
        $watch.Stop()
        $results += [ordered]@{ command = "$($check.Tool) $($arguments -join ' ')"; exitCode = $resultCode; seconds = [math]::Round($watch.Elapsed.TotalSeconds, 2) }
        $results | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $evidencePath 'results.json') -Encoding utf8
        if ($resultCode -ne 0) { throw "Check '$($check.Name)' failed with exit code $resultCode. See $evidencePath." }
    }
}
finally { Pop-Location }
Write-Host "All checks passed. Evidence: $evidencePath"
