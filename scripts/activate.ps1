# Dot source: . .\scripts\activate.ps1
$here  = Split-Path $PSScriptRoot -Parent
$verus = Join-Path $here 'tools\verus'

if (-not (Test-Path (Join-Path $verus 'verus.exe'))) { throw "verus not unpacked under $verus" }

if ($env:PATH -notlike "*$verus*") { $env:PATH = "$verus;$env:PATH" }

# The z3 in the release is the one this verus was tested against. A system z3
# of another version changes which proofs go through, which fails silently.
$z3 = Join-Path $verus 'z3.exe'
if (Test-Path $z3) { $env:VERUS_Z3_PATH = $z3 }

$env:VEXEC_ROOT = $here
Write-Host "verus: $((& (Join-Path $verus 'verus.exe') --version) -join ' ')" -ForegroundColor Green