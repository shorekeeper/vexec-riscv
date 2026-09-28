#Requires -Version 7.0
[CmdletBinding()]
param(
    [string] $Root = (Split-Path $PSScriptRoot -Parent),
    [switch] $Apply
)

$ErrorActionPreference = 'Stop'

$extRoot = Join-Path $env:USERPROFILE '.vscode\extensions'
if (-not (Test-Path $extRoot)) { throw "no extensions directory at $extRoot" }

function Say { param($k, $t, $c = 'Gray') Write-Host ("{0,-12} {1}" -f $k, $t) -ForegroundColor $c }

Write-Host 'installed rust extensions' -ForegroundColor White
$rustExts = Get-ChildItem $extRoot -Directory |
    Where-Object { $_.Name -match 'rust-analyzer|verus|rust-lang' }
foreach ($e in $rustExts) {
    Say '' ("  " + $e.Name) 'DarkGray'
}
if ($rustExts.Count -gt 1) {
    Write-Host ''
    Say 'conflict' 'more than one analyzer: they will both report on the same files' 'DarkYellow'
}

$verusExt = $rustExts | Where-Object { $_.Name -match 'verus' } | Select-Object -Last 1
if (-not $verusExt) { throw 'verus-analyzer is not installed' }

$pkg = Join-Path $verusExt.FullName 'package.json'
$json = Get-Content $pkg -Raw | ConvertFrom-Json

Write-Host ''
Write-Host 'configuration keys it accepts' -ForegroundColor White

$props = $json.contributes.configuration
if ($props -is [array]) {
    $names = @()
    foreach ($block in $props) { $names += $block.properties.PSObject.Properties.Name }
} else {
    $names = $props.properties.PSObject.Properties.Name
}

# Only the ones that matter here: which binary, whether it verifies on save,
# and which cargo target it analyses against.
$interesting = $names | Where-Object {
    $_ -match 'verus|binary|toolchain|cargo\.target|checkOnSave|check\.command|procMacro'
} | Sort-Object
foreach ($n in $interesting) { Say '' ("  " + $n) 'DarkGray' }

$prefix = if ($names[0] -match '^verus-analyzer\.') { 'verus-analyzer' } else { 'rust-analyzer' }
Write-Host ''
Say 'prefix' $prefix 'Green'

# ---------------------------------------------------------------- settings

$verusExe = (Join-Path $Root 'tools\verus\verus.exe') -replace '\\', '/'
$hasVerusBinaryKey = $names | Where-Object { $_ -match 'verusBinaryPath|verus\.binaryPath' } |
    Select-Object -First 1

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('{')
$lines.Add('    // Written by scripts/editor-setup.ps1 from the keys the installed')
$lines.Add('    // extension actually declares.')
$lines.Add('')
$lines.Add('    // verus-analyzer is a fork of rust-analyzer, not an addition to it.')
$lines.Add('    // Both running at once produce two sets of diagnostics on the same')
$lines.Add('    // file, and the stock one does not know what verus! is.')
$lines.Add('    "rust-analyzer.enable": false,')
$lines.Add('')

if ($hasVerusBinaryKey) {
    $lines.Add("    // The verifier this project is pinned to, rather than whichever copy")
    $lines.Add("    // the extension downloaded for itself.")
    $lines.Add("    `"$hasVerusBinaryKey`": `"$verusExe`",")
    $lines.Add('')
}

$lines.Add('    // One target. Analysing against the host would resolve the wrong cfg')
$lines.Add('    // and report imports as missing that are present on riscv.')
$lines.Add("    `"$prefix.cargo.target`": `"riscv64imac-unknown-none-elf`",")
$lines.Add("    `"$prefix.cargo.allTargets`": false,")
$lines.Add('')
$lines.Add('    // Proc macro expansion is required: verus! is one, and without it')
$lines.Add('    // every verified file is a single unparsed token tree.')
$lines.Add("    `"$prefix.procMacro.enable`": true,")
$lines.Add('')
$lines.Add('    // check runs cargo, which does not verify. Verification is a separate')
$lines.Add('    // pass by design and is run by scripts/prove.ps1.')
$lines.Add("    `"$prefix.check.command`": `"check`",")
$lines.Add("    `"$prefix.check.extraArgs`": [`"--target`", `"riscv64imac-unknown-none-elf`"],")
$lines.Add('')
$lines.Add('    "files.watcherExclude": {')
$lines.Add('        "**/target/**": true,')
$lines.Add('        "**/tools/**": true,')
$lines.Add('        "**/.backup/**": true')
$lines.Add('    },')
$lines.Add('    "search.exclude": {')
$lines.Add('        "**/target": true,')
$lines.Add('        "**/tools": true,')
$lines.Add('        "**/.backup": true')
$lines.Add('    }')
$lines.Add('}')

$out = Join-Path $Root '.vscode\settings.json'
Write-Host ''
Say 'target' $out 'White'

if (-not $Apply) {
    Write-Host ''
    Write-Host ($lines -join "`n") -ForegroundColor DarkGray
    Write-Host ''
    Say 'dry run' 'add -Apply to write' 'Yellow'
    exit 0
}

if (Test-Path $out) {
    Copy-Item $out "$out.bak" -Force
    Say 'backup' "$out.bak" 'DarkGray'
}
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
[System.IO.File]::WriteAllText($out, ($lines -join "`n"), [System.Text.UTF8Encoding]::new($false))
Say 'written' $out 'Cyan'

Write-Host ''
Say 'next' 'reload the window: ctrl-shift-p, Developer: Reload Window' 'DarkYellow'