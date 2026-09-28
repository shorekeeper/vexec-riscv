#Requires -Version 7.0
# Fails if anything that is not ours reaches the lock file.
#
# Not a style rule. Every crate in the tree is code inside the artefact that no
# proof covers, and a dependency is never arrived at by decision: it is a
# transitive edge of something that looked convenient at the time.
#
# The verifier is not a dependency. It reads spec/, which cargo never sees, and
# scripts/spec-strip.ps1 produces the plain Rust that is compiled. The proof
# and the build share an algorithm, not a build graph.

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$lock = Join-Path $root 'Cargo.lock'

if (-not (Test-Path $lock)) {
    Write-Host 'no Cargo.lock yet' -ForegroundColor DarkGray
    exit 0
}

$own = @('exec-abi', 'exec-hal', 'exec-core', 'exec', 'esl')
$found = [regex]::Matches((Get-Content $lock -Raw), '(?m)^name\s*=\s*"([^"]+)"') |
    ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique

$foreign = $found | Where-Object { $own -notcontains $_ }

if (-not $foreign) {
    Write-Host ("clean: {0} crates, all ours" -f $found.Count) -ForegroundColor Green
    exit 0
}

Write-Host 'foreign crates in the lock file:' -ForegroundColor Red
$foreign | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
exit 1
