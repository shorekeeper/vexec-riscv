#Requires -Version 7.0
[CmdletBinding()]
param([switch] $Full)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$verus = Join-Path $root 'tools\verus'

if (-not (Test-Path $verus)) { throw "no verus under $verus" }

Write-Host "verus: $((& (Join-Path $verus 'verus.exe') --version) -join ' ')" -ForegroundColor Green
Write-Host ''

Write-Host 'top level' -ForegroundColor White
Get-ChildItem $verus -File | Sort-Object Length -Descending | ForEach-Object {
    Write-Host ("  {0,-40} {1,10:n0} KB" -f $_.Name, ($_.Length / 1KB)) -ForegroundColor DarkGray
}

Write-Host ''
Write-Host 'artifacts cargo would need' -ForegroundColor White
$wanted = @('libbuiltin*.rlib', 'libbuiltin_macros*', 'libvstd*.rlib', 'vstd.vir',
            'libstate_machines_macros*', '*.dll')
foreach ($pattern in $wanted) {
    $hits = Get-ChildItem $verus -Recurse -Filter $pattern -ErrorAction SilentlyContinue
    if ($hits) {
        foreach ($h in $hits) {
            $rel = $h.FullName.Substring($verus.Length + 1)
            Write-Host ("  found   {0,-50} {1,8:n0} KB" -f $rel, ($h.Length / 1KB)) -ForegroundColor Cyan
        }
    } else {
        Write-Host ("  absent  {0}" -f $pattern) -ForegroundColor DarkYellow
    }
}

Write-Host ''
Write-Host 'subdirectories' -ForegroundColor White
Get-ChildItem $verus -Directory | ForEach-Object {
    $n = (Get-ChildItem $_.FullName -Recurse -File).Count
    Write-Host ("  {0,-30} {1} files" -f $_.Name, $n) -ForegroundColor DarkGray
}

if ($Full) {
    Write-Host ''
    Write-Host 'everything' -ForegroundColor White
    Get-ChildItem $verus -Recurse -File | ForEach-Object {
        Write-Host ("  {0}" -f $_.FullName.Substring($verus.Length + 1)) -ForegroundColor DarkGray
    }
}