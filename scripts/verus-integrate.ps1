#Requires -Version 7.0
[CmdletBinding()]
param([switch] $Keep, [switch] $Undo)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$verus = Join-Path $root 'tools\verus'
$target = 'riscv64imac-unknown-none-elf'
$cargoVerus = Join-Path $verus 'cargo-verus.exe'
$wsRoot = Join-Path $verus 'Cargo.toml'
$marker = Join-Path $verus '.synth-workspace'

function Say { param($k, $t, $c = 'Gray') Write-Host ("{0,-10} {1}" -f $k, $t) -ForegroundColor $c }
function Head { param($t) Write-Host ''; Write-Host $t -ForegroundColor White }

if ($Undo) {
    if (Test-Path $marker) {
        Remove-Item $wsRoot -Force -ErrorAction SilentlyContinue
        Remove-Item $marker -Force
        Say 'undone' 'synthesised workspace root removed' 'Cyan'
    } else {
        Say 'undo' 'nothing synthesised' 'DarkGray'
    }
    exit 0
}

$scratch = Join-Path $env:TEMP "vexec-probe-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$results = [ordered]@{}

try {
    Head '0. crates and what they inherit'

    $members = @()
    $inherited = @{}
    foreach ($name in 'builtin', 'builtin_macros', 'vstd', 'state_machines_macros') {
        $m = Join-Path $verus "$name\Cargo.toml"
        if (-not (Test-Path $m)) { continue }
        $text = Get-Content $m -Raw
        $crate = ([regex]::Match($text, 'name\s*=\s*"([^"]+)"')).Groups[1].Value
        $members += $name

        foreach ($hit in [regex]::Matches($text, '(?m)^\s*([a-z0-9_-]+)\.workspace\s*=\s*true')) {
            $inherited[$hit.Groups[1].Value] = $true
        }
        if ($text -match '(?ms)^\s*\[lints\]\s*\r?\n\s*workspace\s*=\s*true') { $inherited['lints'] = $true }

        $paths = [regex]::Matches($text, 'path\s*=\s*"([^"]+)"') | ForEach-Object { $_.Groups[1].Value }
        Say $name ("{0}{1}" -f $crate, $(if ($paths) { "  paths: $($paths -join ', ')" } else { '' })) 'DarkGray'
    }
    Say 'inherits' (($inherited.Keys | Sort-Object) -join ', ') 'DarkYellow'

    Head 'A. a workspace root for tools/verus, then a path dependency'

    if (Test-Path $wsRoot) {
        Say 'root' 'already present, left alone' 'DarkGray'
    } else {
        $lines = @('[workspace]', 'resolver = "2"',
                   'members = [' + (($members | ForEach-Object { "`"$_`"" }) -join ', ') + ']', '')
        if ($inherited['lints']) { $lines += @('[workspace.lints]', '') }
        $pkg = @()
        foreach ($k in $inherited.Keys) {
            switch ($k) {
                'version'      { $pkg += 'version = "0.1.0"' }
                'edition'      { $pkg += 'edition = "2021"' }
                'rust-version' { $pkg += 'rust-version = "1.88"' }
                'license'      { $pkg += 'license = "MIT"' }
                'authors'      { $pkg += 'authors = []' }
                'repository'   { $pkg += 'repository = ""' }
                'homepage'     { $pkg += 'homepage = ""' }
                'description'  { $pkg += 'description = ""' }
                'readme'       { $pkg += 'readme = ""' }
                'keywords'     { $pkg += 'keywords = []' }
                'categories'   { $pkg += 'categories = []' }
                'publish'      { $pkg += 'publish = false' }
            }
        }
        if ($pkg.Count -gt 0) { $lines += @('[workspace.package]') + $pkg + @('') }

        Set-Content $wsRoot ($lines -join "`n")
        Set-Content $marker 'synthesised by verus-integrate.ps1' -NoNewline
        Say 'root' "synthesised, $($members.Count) members" 'Cyan'
    }

    $dirA = Join-Path $scratch 'a'
    New-Item -ItemType Directory -Force -Path (Join-Path $dirA 'src') | Out-Null
    $bm = (Join-Path $verus 'builtin_macros') -replace '\\', '/'
    $bl = (Join-Path $verus 'builtin') -replace '\\', '/'

    @"
[package]
name = "probe"
version = "0.0.0"
edition = "2021"

[lib]
path = "src/lib.rs"
test = false
bench = false

[dependencies]
verus_builtin_macros = { path = "$bm" }
verus_builtin = { path = "$bl" }

[workspace]
"@ | Set-Content (Join-Path $dirA 'Cargo.toml')

    @'
#![no_std]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

use verus_builtin_macros::verus;

verus! {

pub struct Counter { value: u32 }

impl Counter {
    pub closed spec fn get(&self) -> nat { self.value as nat }

    pub fn new() -> (r: Counter)
        ensures r.get() == 0
    {
        Counter { value: 0 }
    }

    pub fn bump(&mut self) -> (r: bool)
        requires old(self).get() < 100
        ensures final(self).get() == old(self).get() + 1, r
    {
        proof { assert(old(self).value < 100); }
        self.value = self.value + 1;
        true
    }
}

}
'@ | Set-Content (Join-Path $dirA 'src\lib.rs')

    Push-Location $dirA
    try {
        $out = & cargo build --target $target --release 2>&1 | Out-String
        if ($LASTEXITCODE -eq 0) {
            Say 'result' 'builds' 'Green'
            $results['A'] = $true

            $lock = Join-Path $dirA 'Cargo.lock'
            if (Test-Path $lock) {
                $names = [regex]::Matches((Get-Content $lock -Raw), '(?m)^name\s*=\s*"([^"]+)"') |
                    ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
                Say 'cost' ("{0} crates, host build only" -f $names.Count) 'DarkYellow'
                Say '' ("  " + ($names -join ', ')) 'DarkGray'
            }

            $rlib = Get-ChildItem (Join-Path $dirA "target\$target\release") -Filter '*.rlib' -ErrorAction SilentlyContinue |
                Select-Object -First 1
            if ($rlib) {
                $ascii = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($rlib.FullName))
                $v = $ascii -match 'vstd'
                Say 'artefact' ("{0:n0} KB, vstd referenced: {1}" -f ($rlib.Length / 1KB), $v) `
                    $(if ($v) { 'DarkYellow' } else { 'Green' })
            }
        } else {
            Say 'result' 'refused' 'DarkYellow'
            $out -split "`n" | Select-Object -First 20 | ForEach-Object {
                if ($_.Trim()) { Say '' "  $($_.TrimEnd())" 'DarkGray' }
            }
            $results['A'] = $false
        }
    } finally { Pop-Location }

    Head 'C. cargo verus build'

    if (Test-Path $cargoVerus) {
        $env:PATH = "$verus;$env:PATH"
        & $cargoVerus verus build --help 2>&1 | Out-String -Stream |
            Select-Object -First 20 | ForEach-Object {
                if ($_.Trim()) { Say '' "  $($_.TrimEnd())" 'DarkGray' }
            }

        $dirC = Join-Path $scratch 'c'
        New-Item -ItemType Directory -Force -Path (Join-Path $dirC 'src') | Out-Null
        @"
[package]
name = "probec"
version = "0.0.0"
edition = "2021"

[lib]
path = "src/lib.rs"
test = false
bench = false

[dependencies]

[workspace]
"@ | Set-Content (Join-Path $dirC 'Cargo.toml')
        Copy-Item (Join-Path $dirA 'src\lib.rs') (Join-Path $dirC 'src\lib.rs')

        Push-Location $dirC
        try {
            foreach ($form in @(
                @('verus', 'build', '--release', '--target', $target),
                @('verus', 'build', '--target', $target),
                @('verus', 'build')
            )) {
                Write-Host ''
                Say 'try' ($form -join ' ') 'DarkGray'
                $o = & $cargoVerus @form 2>&1 | Out-String
                if ($LASTEXITCODE -eq 0) {
                    Say 'result' 'accepted' 'Green'
                    $results['C'] = $true
                    break
                }
                $o -split "`n" | Where-Object { $_ -match 'rror' } | Select-Object -First 3 |
                    ForEach-Object { Say '' "  $($_.Trim())" 'DarkGray' }
            }
            if (-not $results.Contains('C')) { $results['C'] = $false }
        } finally { Pop-Location }
    } else {
        $results['C'] = $false
    }

} finally {
    if (-not $Keep) { Remove-Item $scratch -Recurse -Force -ErrorAction SilentlyContinue }
    else { Write-Host ''; Say 'kept' $scratch 'Cyan' }
}

Head 'summary'
foreach ($k in $results.Keys) {
    Say $k $(if ($results[$k]) { 'works' } else { 'no' }) $(if ($results[$k]) { 'Green' } else { 'DarkGray' })
}
Write-Host ''
Say 'undo' '.\scripts\verus-integrate.ps1 -Undo removes the synthesised root' 'DarkGray'