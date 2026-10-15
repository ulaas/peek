<#
.SYNOPSIS
  Run a cargo command, then keep target/ in check.

.EXAMPLE
  ./scripts/dev.ps1 build
  ./scripts/dev.ps1 test --workspace
  ./scripts/dev.ps1 run -- vnc://127.0.0.1:5999
#>
$CargoArgs = $args

Push-Location (Join-Path $PSScriptRoot '..')
try {
    cargo @CargoArgs
    $code = $LASTEXITCODE
    & (Join-Path $PSScriptRoot 'hygiene.ps1') -MaxGB 12 -StaleDays 3
    exit $code
} finally {
    Pop-Location
}
