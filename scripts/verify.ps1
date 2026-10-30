[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$workspacePath = Split-Path -Parent $PSScriptRoot
$evidencePath = Join-Path $workspacePath '.artifacts\verification'
New-Item -ItemType Directory -Path $evidencePath -Force | Out-Null
$checks = @(
    @{ Name = 'fmt'; Args = @('fmt', '--all', '--', '--check') },
    @{ Name = 'clippy'; Args = @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings') },
    @{ Name = 'test'; Args = @('test', '--workspace') },
    @{ Name = 'build'; Args = @('build', '--workspace') },
    @{ Name = 'release'; Args = @('build', '--release') }
)
$results = @()
Push-Location -LiteralPath $workspacePath
try {
    foreach ($check in $checks) {
        Write-Host "Running cargo $($check.Args -join ' ')"
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $arguments = $check.Args
        & cargo @arguments 2>&1 | Tee-Object -FilePath (Join-Path $evidencePath "$($check.Name).log") | ForEach-Object { Write-Host $_ }
        $resultCode = $LASTEXITCODE
        $watch.Stop()
        $results += [ordered]@{ command = "cargo $($arguments -join ' ')"; exitCode = $resultCode; seconds = [math]::Round($watch.Elapsed.TotalSeconds, 2) }
        $results | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $evidencePath 'results.json') -Encoding utf8
        if ($resultCode -ne 0) { throw "Check '$($check.Name)' failed with exit code $resultCode. See $evidencePath." }
    }
}
finally { Pop-Location }
Write-Host "All checks passed. Evidence: $evidencePath"
