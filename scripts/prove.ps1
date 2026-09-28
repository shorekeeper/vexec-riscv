#Requires -Version 7.0
# Runs the verifier over spec/.
#
# Nothing here is compiled by cargo. The plain Rust that is compiled comes from
# scripts/spec-strip.ps1, which only deletes ghost items; -Check confirms the
# two have not drifted.

[CmdletBinding()]
param(
    [string] $Only = '',
    [int]    $Rlimit = 60,
    [switch] $ShowTriggers
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'activate.ps1')

$specDir = Join-Path $env:VEXEC_ROOT 'spec'
$files = Get-ChildItem $specDir -Filter '*.rs' -ErrorAction SilentlyContinue | Sort-Object Name
if ($Only) { $files = $files | Where-Object { $_.BaseName -like "*$Only*" } }
if (-not $files) { Write-Host 'nothing to verify' -ForegroundColor Yellow; exit 0 }

$failed = 0
$totalMs = 0

foreach ($f in $files) {
    Write-Host ("--- {0}" -f $f.Name) -ForegroundColor White

    [string[]] $a = @($f.FullName, '--crate-type=lib', '--time', '--rlimit', $Rlimit)
    if (-not $ShowTriggers) { $a += @('--triggers-mode', 'silent') }

    $out = & verus @a 2>&1 | Out-String
    if ($LASTEXITCODE -eq 0) {
        $r = ($out -split "`n" | Select-String 'verification results')
        Write-Host ("  {0}" -f ($r -replace '\s+', ' ').Trim()) -ForegroundColor Green
    } else {
        Write-Host '  failed' -ForegroundColor Red
        $out -split "`n" | Where-Object { $_ -match '^error|^\s+-->' } |
            Select-Object -First 20 | ForEach-Object {
                Write-Host ("  {0}" -f $_.TrimEnd()) -ForegroundColor DarkYellow
            }
        $failed++
    }

    $smt = ($out -split "`n" | Select-String 'total smt-run')
    if ($smt) {
        $ms = [regex]::Match($smt, '(\d+)\s*ms')
        if ($ms.Success) {
            $totalMs += [int]$ms.Groups[1].Value
            Write-Host ("  smt {0} ms" -f $ms.Groups[1].Value) -ForegroundColor DarkGray
        }
    }
}

Write-Host ''
if ($failed -eq 0) {
    Write-Host ("all {0} proved, {1} ms in the solver" -f $files.Count, $totalMs) -ForegroundColor Green
} else {
    Write-Host ("{0} of {1} failed" -f $failed, $files.Count) -ForegroundColor Red
    exit 1
}
