#Requires -Version 7.0
<#
    Builds the kernel image.

    The profile switch is -Dev rather than -Debug: CmdletBinding supplies a
    common parameter of that name, and declaring another collides with it.
#>
[CmdletBinding()]
param(
    [switch] $Dev,
    [switch] $Bin,
    [switch] $Size
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    # Declared as an array and assigned in place. An `if` used as an expression
    # returns through the pipeline, which unrolls a one element array into the
    # element itself, and splatting a bare string passes it one character at a
    # time.
    [string[]] $profileArgs = @()
    if (-not $Dev) {
        $profileArgs = @('--release')
    }
    $dir = if ($Dev) { 'debug' } else { 'release' }

    # Verification first. A generated file that matches its source proves
    # nothing if the source itself was never checked, and the strip runs
    # regardless of whether it was: without this, a failed proof and a green
    # build are the same thing.
    if (Test-Path (Join-Path $root 'tools\verus\verus.exe')) {
        & (Join-Path $PSScriptRoot 'prove.ps1') | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Write-Host 'verification failed, not building' -ForegroundColor Red
            Write-Host '  .\scripts\prove.ps1' -ForegroundColor DarkGray
            exit 1
        }
        Write-Host 'proofs ok' -ForegroundColor DarkGray
    } else {
        Write-Host 'verus absent: building unverified' -ForegroundColor Yellow
    }

    $pairs = @(
        @('spec/queue.rs',   'crates/exec-core/src/queue.rs'),
        @('spec/pmm.rs',     'crates/exec-core/src/pmm.rs'),
        @('spec/objects.rs', 'crates/exec-core/src/objects.rs'),
        @('spec/window.rs',  'crates/exec-core/src/window.rs')
    )
    foreach ($p in $pairs) {
        if (-not (Test-Path (Join-Path $root $p[0]))) { continue }
        & (Join-Path $PSScriptRoot 'spec-strip.ps1') -In $p[0] -Out $p[1] -Check | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Write-Host ("out of sync: {0} does not match {1}" -f $p[1], $p[0]) -ForegroundColor Red
            Write-Host ("  .\scripts\spec-strip.ps1 -In {0} -Out {1}" -f $p[0], $p[1]) -ForegroundColor DarkGray
            exit 1
        }
    }

    cargo build @profileArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    $elf = Join-Path $root "target\riscv64imac-unknown-none-elf\$dir\exec"
    if (-not (Test-Path $elf)) {
        Write-Host 'built, but no exec image present' -ForegroundColor Yellow
        exit 0
    }

    Write-Host ("image: {0}  ({1:n0} KB)" -f $elf, ((Get-Item $elf).Length / 1KB)) -ForegroundColor Cyan

    if ($Bin -or $Size) {
        # llvm-objcopy and llvm-size ship with the llvm-tools component and are
        # found by path. cargo-binutils wraps exactly these and costs fifty
        # crates to save one Join-Path.
        $rh = (rustup show home).Trim()
        $tc = ((rustup show active-toolchain) -split ' ')[0]
        $bin = Join-Path $rh "toolchains\$tc\lib\rustlib\x86_64-pc-windows-msvc\bin"

        if ($Size) {
            $sz = Join-Path $bin 'llvm-size.exe'
            if (Test-Path $sz) {
                & $sz $elf
            } else {
                Write-Host "llvm-size absent: rustup component add llvm-tools --toolchain $tc" -ForegroundColor Yellow
            }
        }

        if ($Bin) {
            $oc = Join-Path $bin 'llvm-objcopy.exe'
            if (-not (Test-Path $oc)) {
                throw "llvm-objcopy absent: rustup component add llvm-tools --toolchain $tc"
            }
            $out = Join-Path $root 'artifacts\exec.bin'
            New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
            & $oc -O binary $elf $out
            Write-Host ("flat:  {0}  ({1:n0} KB)" -f $out, ((Get-Item $out).Length / 1KB)) -ForegroundColor Cyan
        }
    }
} finally {
    Pop-Location
}