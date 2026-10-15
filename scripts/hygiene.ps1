<#
.SYNOPSIS
  Keeps the cargo target directory from growing without bound.

.DESCRIPTION
  Cargo never deletes build outputs for superseded crate hashes (old dep
  versions, changed features, edited code in incremental sessions), so
  target/ grows forever. This script:

  1. In deps/, build/, .fingerprint/ and incremental/ groups entries by crate
     name. The newest hash of each crate is always kept; older hashes are
     removed once they are older than -StaleDays days.
  2. If target/ is still larger than -MaxGB, runs `cargo clean`.

  Safe to run whenever no cargo process is running.

.EXAMPLE
  ./scripts/hygiene.ps1                  # defaults: 3 days, 12 GB cap
  ./scripts/hygiene.ps1 -MaxGB 6 -StaleDays 1
#>
param(
    [int]$MaxGB = 12,
    [int]$StaleDays = 3,
    [string]$Target = (Join-Path $PSScriptRoot '..\target')
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Target)) { Write-Host "hygiene: no target dir, nothing to do"; return }
$Target = (Resolve-Path $Target).Path

function Get-DirBytes([string]$Path) {
    $s = (Get-ChildItem -LiteralPath $Path -Recurse -File -Force -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum).Sum
    if ($null -eq $s) { 0 } else { $s }
}

$before = Get-DirBytes $Target
$cutoff = (Get-Date).AddDays(-$StaleDays)
$pattern = '^(?:lib)?(?<name>.+?)-(?<hash>[0-9a-f]{16})(?:\..*)?$'
$removed = 0

# Profile dirs: target/debug, target/release, target/<triple>/debug ...
$profileDirs = Get-ChildItem -LiteralPath $Target -Directory -Recurse -Depth 1 -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName '.fingerprint') }

foreach ($profileDir in $profileDirs) {
    foreach ($sub in 'deps', 'build', '.fingerprint', 'incremental') {
        $dir = Join-Path $profileDir.FullName $sub
        if (-not (Test-Path $dir)) { continue }
        $entries = foreach ($e in Get-ChildItem -LiteralPath $dir -Force) {
            if ($e.Name -match $pattern) {
                [pscustomobject]@{ Item = $e; Name = $Matches.name; Hash = $Matches.hash }
            }
        }
        foreach ($group in $entries | Group-Object Name) {
            # newest write time per hash
            $byHash = $group.Group | Group-Object Hash | ForEach-Object {
                [pscustomobject]@{
                    Hash  = $_.Name
                    Time  = ($_.Group.Item | Measure-Object LastWriteTime -Maximum).Maximum
                    Items = $_.Group.Item
                }
            } | Sort-Object Time -Descending
            $byHash | Select-Object -Skip 1 | Where-Object { $_.Time -lt $cutoff } | ForEach-Object {
                foreach ($item in $_.Items) {
                    Remove-Item -LiteralPath $item.FullName -Recurse -Force -ErrorAction SilentlyContinue
                    $removed++
                }
            }
        }
    }
}

$after = Get-DirBytes $Target
if ($after -gt ($MaxGB * 1GB)) {
    Write-Host ("hygiene: target is {0:N1} GB (> {1} GB cap), running cargo clean" -f ($after / 1GB), $MaxGB)
    Push-Location (Join-Path $PSScriptRoot '..')
    try { cargo clean } finally { Pop-Location }
    $after = 0
}

Write-Host ("hygiene: {0:N2} GB -> {1:N2} GB ({2} superseded entries removed)" -f ($before / 1GB), ($after / 1GB), $removed)
