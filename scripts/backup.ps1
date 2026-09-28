#Requires -Version 7.0
[CmdletBinding()]
param(
    [string] $Tag = '',
    [switch] $List,
    [string] $Restore = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$store = Join-Path $root '.backup'

if ($List) {
    if (-not (Test-Path $store)) { Write-Host 'no backups' -ForegroundColor DarkGray; exit 0 }
    Get-ChildItem $store -Directory | Sort-Object Name -Descending | ForEach-Object {
        $n = (Get-ChildItem $_.FullName -Recurse -File).Count
        Write-Host ("  {0,-40} {1,4} files" -f $_.Name, $n) -ForegroundColor Cyan
    }
    exit 0
}

if ($Restore) {
    $from = Join-Path $store $Restore
    if (-not (Test-Path $from)) { throw "no such backup: $Restore" }
    Get-ChildItem $from -Recurse -File | ForEach-Object {
        $rel = $_.FullName.Substring($from.Length + 1)
        $to = Join-Path $root $rel
        New-Item -ItemType Directory -Force -Path (Split-Path $to) | Out-Null
        Copy-Item $_.FullName $to -Force
        Write-Host "  restored $rel" -ForegroundColor Cyan
    }
    Write-Host 'done.' -ForegroundColor Cyan
    exit 0
}

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$name = if ($Tag) { "$stamp-$Tag" } else { $stamp }
$dest = Join-Path $store $name

$sources = @('crates', 'boot', 'spec', 'scripts', 'Cargo.toml', '.cargo', 'rust-toolchain.toml')
$copied = 0

foreach ($s in $sources) {
    $path = Join-Path $root $s
    if (-not (Test-Path $path)) { continue }

    if (Test-Path $path -PathType Leaf) {
        $to = Join-Path $dest $s
        New-Item -ItemType Directory -Force -Path (Split-Path $to) | Out-Null
        Copy-Item $path $to -Force
        $copied++
        continue
    }

    Get-ChildItem $path -Recurse -File | ForEach-Object {
        $rel = $_.FullName.Substring($root.Length + 1)
        $to = Join-Path $dest $rel
        New-Item -ItemType Directory -Force -Path (Split-Path $to) | Out-Null
        Copy-Item $_.FullName $to -Force
        $script:copied++
    }
}

Write-Host ("backup {0}: {1} files" -f $name, $copied) -ForegroundColor Green
Write-Host ("  {0}" -f $dest) -ForegroundColor DarkGray
Write-Host ''
Write-Host '  .\scripts\backup.ps1 -List' -ForegroundColor DarkGray
Write-Host ("  .\scripts\backup.ps1 -Restore {0}" -f $name) -ForegroundColor DarkGray