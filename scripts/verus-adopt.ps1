#Requires -Version 7.0
[CmdletBinding()]
param(
    [string] $Root = (Split-Path $PSScriptRoot -Parent),
    [switch] $Apply
)

$ErrorActionPreference = 'Stop'

$verus = Join-Path $Root 'tools\verus'
$vstdManifest = Join-Path $verus 'vstd\Cargo.toml'
if (-not (Test-Path $vstdManifest)) { throw "no vstd manifest under $verus" }

# Read the version from the shipped manifest rather than hardcoding it. It has
# to match the verifier binary exactly, and the binary is what put this file
# on disk.
$version = ([regex]::Match((Get-Content $vstdManifest -Raw), '(?m)^version\s*=\s*"([^"]+)"')).Groups[1].Value
if (-not $version) { throw 'could not read the vstd version' }
Write-Host ("vstd version: {0}" -f $version) -ForegroundColor Green
Write-Host ''

$plan = [System.Collections.Generic.List[object]]::new()

function Plan {
    param([string] $What, [string] $Detail, [scriptblock] $Do)
    $plan.Add([pscustomobject]@{ What = $What; Detail = $Detail; Do = $Do })
    Write-Host ("ready    {0,-14} {1}" -f $What, $Detail) -ForegroundColor Green
}

# ---------------------------------------------------------------- manifest

$coreManifest = Join-Path $Root 'crates\exec-core\Cargo.toml'
$current = Get-Content $coreManifest -Raw

if ($current -match 'vstd') {
    Write-Host 'skip     manifest       already carries vstd' -ForegroundColor DarkGray
} else {
    $block = @"
exec-abi = { workspace = true }

# The verification toolchain. Present in the target graph because vstd is an
# ordinary crate that has to compile for riscv, and absent from the artefact
# because without the verus_keep_ghost cfg the macro erases every ghost item
# and what is left references nothing.
#
# default-features off: vstd defaults to std, which does not exist here.
#
# Versions are exact and must match the verifier binary under tools/verus. A
# mismatch is not a compatibility question, it is a different language.
vstd = { version = "=$version", default-features = false }
verus_builtin = { version = "=$version" }
verus_builtin_macros = { version = "=$version" }
"@
    Plan 'manifest' 'exec-core gains vstd, builtin, macros' {
        $text = Get-Content $coreManifest -Raw
        $text = $text -replace '(?m)^exec-abi = \{ workspace = true \}\s*$', $block
        [System.IO.File]::WriteAllText($coreManifest, $text, [System.Text.UTF8Encoding]::new($false))
        Write-Host "written  crates/exec-core/Cargo.toml" -ForegroundColor Cyan
    }.GetNewClosure()
}

# ---------------------------------------------------------------- move

$from = Join-Path $Root 'spec\queue.rs'
$to = Join-Path $Root 'crates\exec-core\src\queue.rs'

if (-not (Test-Path $from)) {
    Write-Host 'skip     queue.rs       not in spec, already moved' -ForegroundColor DarkGray
} else {
    Plan 'queue.rs' 'spec -> crates/exec-core/src' {
        Move-Item $from $to -Force
        Write-Host "moved    crates/exec-core/src/queue.rs" -ForegroundColor Cyan
    }.GetNewClosure()
}

# ---------------------------------------------------------------- module

$coreLib = Join-Path $Root 'crates\exec-core\src\lib.rs'
if ((Get-Content $coreLib -Raw) -match '(?m)^pub mod queue;') {
    Write-Host 'skip     lib.rs         queue already declared' -ForegroundColor DarkGray
} else {
    Plan 'lib.rs' 'declare the queue module' {
        $text = Get-Content $coreLib -Raw
        # Declared but not yet used by anything. The modules it replaces stay
        # until their call sites are moved over, which is a separate change.
        $text = $text -replace '(?m)^pub mod platform;', "pub mod platform;`npub mod queue;"
        [System.IO.File]::WriteAllText($coreLib, $text, [System.Text.UTF8Encoding]::new($false))
        Write-Host "written  crates/exec-core/src/lib.rs" -ForegroundColor Cyan
    }.GetNewClosure()
}

# ---------------------------------------------------------------- audit

$audit = Join-Path $Root 'scripts\audit.ps1'
Plan 'audit.ps1' 'three buckets instead of two' {
    @'
#Requires -Version 7.0
# Reports what is in the lock file, in three buckets.
#
# Ours              written here.
# Verification      the verus toolchain. In the graph because vstd compiles for
#                   the target; not in the artefact because the macro erases
#                   every ghost item when verus_keep_ghost is off. Nothing in
#                   this bucket is called by kernel code.
# Unexpected        anything else, and a failure.
#
# The point of the third bucket is that a dependency is never arrived at by
# decision: it is a transitive edge of something that looked convenient.

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$lock = Join-Path $root 'Cargo.lock'

if (-not (Test-Path $lock)) {
    Write-Host 'no Cargo.lock yet' -ForegroundColor DarkGray
    exit 0
}

$ours = @('exec-abi', 'exec-hal', 'exec-core', 'exec', 'esl')

# The verus crates and the proc macro machinery they rest on. Listed rather
# than inferred, so a new edge shows up as unexpected and gets looked at.
$verification = @(
    'vstd', 'verus_builtin', 'verus_builtin_macros',
    'verus_state_machines_macros', 'verus_syn', 'verus_prettyplease',
    'syn', 'quote', 'proc-macro2', 'unicode-ident',
    'synstructure', 'convert_case', 'paste', 'unicode-segmentation'
)

$found = [regex]::Matches((Get-Content $lock -Raw), '(?m)^name\s*=\s*"([^"]+)"') |
    ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique

$mine = $found | Where-Object { $ours -contains $_ }
$tool = $found | Where-Object { $verification -contains $_ }
$other = $found | Where-Object { $ours -notcontains $_ -and $verification -notcontains $_ }

Write-Host ("ours          {0}" -f ($mine -join ', ')) -ForegroundColor Green
Write-Host ("verification  {0} crates" -f $tool.Count) -ForegroundColor DarkGray
if ($tool.Count -gt 0) {
    Write-Host ("              {0}" -f ($tool -join ', ')) -ForegroundColor DarkGray
}

if ($other.Count -eq 0) {
    Write-Host ''
    Write-Host ("clean: {0} crates, none unaccounted for" -f $found.Count) -ForegroundColor Green
    exit 0
}

Write-Host ''
Write-Host 'unexpected crates:' -ForegroundColor Red
$other | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
exit 1
'@ | Set-Content $audit -Encoding utf8
    Write-Host "written  scripts/audit.ps1" -ForegroundColor Cyan
}.GetNewClosure()

# ---------------------------------------------------------------- prove

$prove = Join-Path $Root 'scripts\prove.ps1'
Plan 'prove.ps1' 'find verus files wherever they live' {
    @'
#Requires -Version 7.0
# Runs the verifier over every file containing a verus block.
#
# Found by content rather than by directory: a verified module lives in the
# crate it belongs to, and spec/ holds only the ones not yet adopted.

[CmdletBinding()]
param(
    [string] $Only = '',
    [int]    $Rlimit = 60,
    [switch] $ShowTriggers
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'activate.ps1')

$root = $env:VEXEC_ROOT
$files = @()
foreach ($dir in 'crates', 'spec') {
    $path = Join-Path $root $dir
    if (-not (Test-Path $path)) { continue }
    $files += Get-ChildItem $path -Recurse -Filter '*.rs' |
        Where-Object { (Get-Content $_.FullName -Raw) -match '(?m)^verus!\s*\{' }
}
$files = $files | Sort-Object FullName
if ($Only) { $files = $files | Where-Object { $_.Name -like "*$Only*" } }

if (-not $files) { Write-Host 'nothing to verify' -ForegroundColor Yellow; exit 0 }

$failed = 0
$totalMs = 0

foreach ($f in $files) {
    $rel = $f.FullName.Substring($root.Length + 1)
    Write-Host ("--- {0}" -f $rel) -ForegroundColor White

    [string[]] $a = @($f.FullName, '--crate-type=lib', '--time', '--rlimit', $Rlimit)
    if (-not $ShowTriggers) { $a += @('--triggers-mode', 'silent') }

    $out = & verus @a 2>&1 | Out-String
    $code = $LASTEXITCODE

    if ($code -eq 0) {
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
'@ | Set-Content $prove -Encoding utf8
    Write-Host "written  scripts/prove.ps1" -ForegroundColor Cyan
}.GetNewClosure()

# ---------------------------------------------------------------- driver

Write-Host ''
Write-Host ("{0} changes planned" -f $plan.Count)

if (-not $Apply) {
    Write-Host ''
    Write-Host 'dry run, nothing written. add -Apply.' -ForegroundColor Yellow
    exit 0
}

Write-Host ''
foreach ($p in $plan) { & $p.Do }

Write-Host ''
Write-Host 'done.' -ForegroundColor Cyan
Write-Host ''
Write-Host '  .\scripts\build.ps1' -ForegroundColor DarkGray
Write-Host '  .\scripts\audit.ps1' -ForegroundColor DarkGray
Write-Host '  .\scripts\prove.ps1' -ForegroundColor DarkGray